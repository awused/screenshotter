use core::slice;
use std::cmp::{max, min};

use wayland_client::Dispatch;
use wayland_client::protocol::wl_buffer::{self, WlBuffer};

use crate::util::MRegion;
use crate::wayland::protos::Buffer;
use crate::wayland::{OutputKey, State};

#[derive(Debug, Clone, Copy)]
pub struct DrawingKey(pub OutputKey, pub usize);

#[derive(Debug)]
pub struct Drawing {
    pub buffer: Buffer,
    pub drawn: Option<MRegion>,
    // The only buffer (right now) that is continually mutated
    pub locked: bool,
}

impl Drawing {
    // Region is in buffer coordinates
    pub fn draw(&mut self, region: Option<MRegion>) -> bool {
        if region == self.drawn {
            return false;
        }

        if self.locked {
            warn!("Tried to write to locked buffer, handle deferring the drawing");
            return false;
        }
        self.locked = true;

        // For now just be lazy and only handle 32bit pixels
        assert!(self.buffer.format.size() == 4);

        // TODO -- formats -- this assumes Argb8888
        if let Some(drawn) = &self.drawn.take() {
            // Could skip this if drawn fits inside the new region
            unsafe {
                self.rect(drawn, 0, 0);
            }
        }

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
            for y in region.y as usize..(region.y + region.height) as usize {
                let start = region.x as usize + y * stride;
                assert!(start + (region.width as usize) <= max_size);
                let start = raw.add(start);
                assert!(start.is_aligned());
                slice::from_raw_parts_mut(start, region.width as _).fill(fill)
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

impl Dispatch<WlBuffer, State> for DrawingKey {
    fn event(
        &self,
        state: &mut State,
        proxy: &WlBuffer,
        event: <WlBuffer as wayland_client::Proxy>::Event,
        conn: &wayland_client::Connection,
        qhandle: &wayland_client::QueueHandle<State>,
    ) {
        if !matches!(wl_buffer::Event::Release, event) {
            return;
        }

        state.outputs.get_mut(&self.0).unwrap().overlay.get_mut().unwrap().drawings[self.1]
            .locked = false;
    }
}
