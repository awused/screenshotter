use std::cell::OnceCell;
use std::ffi::{CString, c_void};
use std::os::fd::{BorrowedFd, RawFd};
use std::process;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use color_eyre::Result;
use color_eyre::eyre::{OptionExt, bail};
use image::RgbaImage;
use libc::{MAP_SHARED, O_CREAT, O_EXCL, O_RDWR, PROT_READ, PROT_WRITE, close, ftruncate, shm_open, shm_unlink};
use nix::errno::Errno;
use wayland_client::NoopIgnore;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_shm::WlShm;
use wayland_protocols::ext::image_capture_source::v1::client::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;

use crate::wayland::{Format, State};

#[derive(Debug)]
pub struct Buffer {
    pub wl_buffer: WlBuffer,
    pub buf: *mut c_void,
    pub fd: RawFd,
    format: Format,
    width: usize,
    height: usize,
    buf_size: usize,
}

impl Buffer {
    // Only handle 8 bit rgba for now
    pub fn read(&self) -> Result<RgbaImage> {
        let start = Instant::now();
        let size = self.width * self.height * 4;
        let mut out = vec![0u8; size];

        assert_eq!(self.format, Format::Argb8888);
        assert!(size <= self.buf_size, "Image buffer larger than output buffer");
        unsafe {
            self.buf.copy_to_nonoverlapping(out.as_mut_ptr().cast(), size);
        }

        out.chunks_exact_mut(4).for_each(|c| c.swap(0, 2));
        println!("{:?}", start.elapsed());
        RgbaImage::from_raw(self.width as _, self.height as _, out)
            .ok_or_eyre("Can't construct image")
    }
}

#[derive(Debug, Default)]
pub struct Protos {
    pub compositor: OnceCell<WlCompositor>,
    pub fractional: OnceCell<WpFractionalScaleManagerV1>,
    pub viewporter: OnceCell<WpViewporter>,
    pub layer_shell: OnceCell<ZwlrLayerShellV1>,
    pub shm: OnceCell<WlShm>,
    pub output_capture: OnceCell<ExtOutputImageCaptureSourceManagerV1>,
    pub image_copy: OnceCell<ExtImageCopyCaptureManagerV1>,
}

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

impl Protos {
    pub fn create_buffer(
        &self,
        qhandle: &wayland_client::QueueHandle<State>,
        format: Format,
        width: i32,
        height: i32,
    ) -> Result<Buffer> {
        // If this runs into problems, we'll need rng
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let name = CString::new(format!("screenshotter-{}-{}", process::id(), id)).unwrap();

        let stride = width.checked_mul(format.size() as i32).ok_or_eyre("Buffer too large")?;
        let size = height.checked_mul(stride).ok_or_eyre("Buffer too large")?;

        let fd = unsafe { shm_open(name.as_ptr(), O_RDWR | O_CREAT | O_EXCL, 0o600) };
        if fd < 0 {
            bail!("Unable to open shared memory: {fd}");
        }

        let buf = unsafe {
            shm_unlink(name.as_ptr());

            let mut ret = 1;
            for _ in 0..100 {
                ret = ftruncate(fd, size as i64);
                if ret == 0 || Errno::last() != Errno::EINTR {
                    break;
                }
            }
            if ret < 0 {
                close(fd);
                bail!("Failed to extend file descriptor to {}: {}", size, ret);
            }

            libc::mmap(std::ptr::null_mut(), size as _, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0)
        };

        let pool = self.shm().create_pool(
            unsafe { BorrowedFd::borrow_raw(fd) },
            size as _,
            qhandle,
            NoopIgnore,
        );

        let wl_buffer = pool.create_buffer(
            0,
            width as _,
            height as _,
            stride,
            format.wl_format(),
            qhandle,
            NoopIgnore,
        );
        pool.destroy();

        Ok(Buffer {
            wl_buffer,
            buf,
            fd: fd as _,
            format,
            width: width as _,
            height: height as _,
            buf_size: size as _,
        })
    }
}

macro_rules! proto_get {
    ($x:ident, $t:ty) => {
        impl Protos {
            pub fn $x(&self) -> &$t {
                self.$x.get().unwrap()
            }
        }
    };
}

proto_get!(compositor, WlCompositor);
proto_get!(fractional, WpFractionalScaleManagerV1);
proto_get!(viewporter, WpViewporter);
proto_get!(layer_shell, ZwlrLayerShellV1);
proto_get!(shm, WlShm);
proto_get!(output_capture, ExtOutputImageCaptureSourceManagerV1);
proto_get!(image_copy, ExtImageCopyCaptureManagerV1);

impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe {
            close(self.fd);
            libc::munmap(self.buf.cast(), self.buf_size);
            self.wl_buffer.destroy();
        }
    }
}
