use core::slice;
use std::ffi::{CString, c_void};
use std::os::fd::{BorrowedFd, RawFd};
use std::process;
use std::sync::atomic::{AtomicUsize, Ordering};

use color_eyre::Result;
use color_eyre::eyre::{OptionExt, bail};
use image::Rgb;
use libc::{
    MAP_SHARED, O_CREAT, O_EXCL, O_RDWR, PROT_READ, PROT_WRITE, close, ftruncate, shm_open,
    shm_unlink,
};
use nix::errno::Errno;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::{Dispatch, NoopIgnore};

use crate::wayland::protos::Protos;
use crate::wayland::{Format, State};

#[derive(Debug)]
pub struct Buffer {
    pub wl_buffer: WlBuffer,
    pub buf: *mut c_void,
    pub fd: RawFd,
    pub format: Format,
    // These are untransformed
    pub width: usize,
    pub height: usize,
    // Buffer size in bytes
    pub buf_size: usize,
}

// It's safe to access the buffer from multiple threads, but not if it's being written to on the
// wayland end.
unsafe impl Sync for Buffer {}

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

impl Buffer {
    pub unsafe fn read_rgb(&self, x: i32, y: i32) -> Rgb<u8> {
        debug_assert!(
            x >= 0 && x < self.width as _ && y >= 0 && y < self.height as _,
            "Got bad pixel {x},{y} in {}x{}",
            self.width,
            self.height
        );
        let start = (self.width * y as usize + x as usize) * self.format.size();

        let mut out = [0u8; 3];

        // Both supported formats are u8 and only care about the first three bytes
        unsafe {
            self.buf.cast::<u8>().add(start).copy_to_nonoverlapping(out.as_mut_ptr(), 3);
        }

        match self.format {
            Format::Argb8888 => out.swap(0, 2),
            Format::Bgr888 => {}
        }

        out.into()
    }

    pub unsafe fn read_normal_row(&self, x: i32, y: i32, row: &mut [u8]) {
        assert!(x >= 0 && x < self.width as _ && y >= 0 && y < self.height as _);
        let start = (self.width * y as usize + x as usize) * self.format.size();

        assert!(row.len().is_multiple_of(3));

        let read_size = row.len() / 3 * self.format.size();

        assert!(start + read_size <= self.buf_size);
        let read =
            unsafe { slice::from_raw_parts_mut(self.buf.cast::<u8>().add(start), read_size) };

        match self.format {
            Format::Argb8888 => {
                for (r, c) in row.chunks_exact_mut(3).zip(read.chunks_exact(4)) {
                    r[0] = c[2];
                    r[1] = c[1];
                    r[2] = c[0];
                }
            }
            Format::Bgr888 => row.copy_from_slice(read),
        }
    }

    pub fn new(
        protos: &Protos,
        qhandle: &wayland_client::QueueHandle<State>,
        format: Format,
        width: i32,
        height: i32,
        udata: impl Dispatch<WlBuffer, State> + Send + Sync + 'static,
    ) -> Result<Self> {
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

        let pool = protos.shm().create_pool(
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
            udata,
        );
        pool.destroy();

        Ok(Self {
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

impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe {
            close(self.fd);
            libc::munmap(self.buf.cast(), self.buf_size);
            self.wl_buffer.destroy();
        }
    }
}
