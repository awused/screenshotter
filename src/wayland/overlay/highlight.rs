use core::slice;
use std::cmp::{max, min};

use rayon::iter::{ParallelBridge, ParallelIterator};
use wayland_client::Dispatch;
use wayland_client::protocol::wl_buffer::{self, WlBuffer};

use crate::util::MRegion;
use crate::wayland::buffer::Buffer;
use crate::wayland::{OutputKey, State};

#[derive(Debug, Clone, Copy)]
pub struct HighlightKey(pub OutputKey, pub usize);

#[derive(Debug)]
pub struct Highlight {
    pub buffer: Buffer,
    // Could use a dedicated None buffer for better reuse
    pub drawn: Option<MRegion>,
    pub locked: bool,
}

impl Highlight {
    // Region is in buffer coordinates
    pub fn draw(&mut self, region: Option<MRegion>, force: bool) -> bool {
        assert!(!self.locked, "Tried to write to locked buffer, should not happen");

        if region == self.drawn {
            self.locked = force;
            return force;
        }
        self.locked = true;


        // For now just be lazy and only handle 32bit pixels
        assert!(self.buffer.format.size() == 4);

        if let Some(drawn) = &self.drawn.take()
            && region.is_none_or(|new| !new.fully_contains(drawn))
        {
            unsafe {
                self.rect(drawn, 0, 0);
            }
        }

        // TODO[HDR] -- this assumes Argb8888
        if let Some(drawn) = region {
            unsafe {
                self.rect(
                    &drawn,
                    u32::from_le_bytes([178, 178, 0, 178]),
                    u32::from_le_bytes([255, 255, 0, 255]),
                );
            }
            self.drawn = Some(drawn);
        }


        true
    }

    unsafe fn rect(&mut self, region: &MRegion, fill: u32, border: u32) {
        let raw = self.buffer.buf.cast::<u32>();
        let max_size = self.buffer.width * self.buffer.height;

        let stride = self.buffer.width;

        // Fill
        unsafe {
            if region.width as usize == self.buffer.width {
                let start = region.y as usize * stride;
                let len = region.height as usize * stride;
                assert!(start + len <= max_size);
                let start = raw.add(region.y as usize * stride);
                assert!(start.is_aligned());

                slice::from_raw_parts_mut(start, len).fill(fill);
            } else {
                let buf = slice::from_raw_parts_mut(raw, max_size);
                // Doing this from multiple threads with par_bridge() is just barely faster by
                // enough to be even a little bit useful.
                buf.chunks_exact_mut(stride)
                    .skip(region.y as _)
                    .take(region.height as _)
                    .par_bridge()
                    .for_each(|row| {
                        row[region.x as usize..(region.x + region.width) as usize].fill(fill);
                    });
            }
        }

        // Borders
        unsafe {
            let top = max(0, region.y - 1) as usize;
            let bottom = min(self.buffer.height as _, region.y + region.height + 1) as usize;
            if region.x > 0 {
                for y in top..bottom {
                    raw.add(y * stride + region.x as usize - 1).write(border);
                }
            }

            if region.x + region.width < self.buffer.width as _ {
                let x = (region.x + region.width) as usize;
                for y in top..bottom {
                    raw.add(y * stride + x).write(border);
                }
            }


            let left = max(0, region.x - 1) as usize;
            let right = min(self.buffer.width as _, region.x + region.width + 1) as usize;
            assert!(right >= left);
            let len = right - left;

            if region.y > 0 {
                let start = (region.y - 1) as usize * stride + left as usize;
                assert!(start + len <= max_size);
                let start = raw.add(start);
                assert!(start.is_aligned());
                slice::from_raw_parts_mut(start, len).fill(border);
            }

            if region.y + region.height < self.buffer.height as _ {
                let start = (region.y + region.height) as usize * stride + left as usize;
                assert!(start + len <= max_size);
                let start = raw.add(start);
                assert!(start.is_aligned());
                slice::from_raw_parts_mut(start, len).fill(border);
            }
        }
    }
}

impl Dispatch<WlBuffer, State> for HighlightKey {
    fn event(
        &self,
        state: &mut State,
        _proxy: &WlBuffer,
        event: <WlBuffer as wayland_client::Proxy>::Event,
        _conn: &wayland_client::Connection,
        _qhandle: &wayland_client::QueueHandle<State>,
    ) {
        if !matches!(event, wl_buffer::Event::Release) {
            return;
        }

        state.outputs.get_mut(&self.0).unwrap().overlay.get_mut().unwrap().highlights[self.1]
            .locked = false;
    }
}
