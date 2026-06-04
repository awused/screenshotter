use std::cell::OnceCell;

use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_shm::WlShm;
use wayland_client::protocol::wl_subcompositor::WlSubcompositor;
use wayland_protocols::ext::image_capture_source::v1::client::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::pointer_warp::v1::client::wp_pointer_warp_v1::WpPointerWarpV1;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;


#[derive(Debug, Default)]
pub struct Protos {
    pub compositor: OnceCell<WlCompositor>,
    pub subcompositor: OnceCell<WlSubcompositor>,
    pub fractional: OnceCell<WpFractionalScaleManagerV1>,
    pub viewporter: OnceCell<WpViewporter>,
    pub layer_shell: OnceCell<ZwlrLayerShellV1>,
    pub shm: OnceCell<WlShm>,
    pub output_capture: OnceCell<ExtOutputImageCaptureSourceManagerV1>,
    pub image_copy: OnceCell<ExtImageCopyCaptureManagerV1>,
    pub xdg_output: OnceCell<ZxdgOutputManagerV1>,
    pub shape_manager: OnceCell<WpCursorShapeManagerV1>,
    pub pointer_warp: OnceCell<WpPointerWarpV1>,
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
proto_get!(subcompositor, WlSubcompositor);
proto_get!(fractional, WpFractionalScaleManagerV1);
proto_get!(viewporter, WpViewporter);
proto_get!(layer_shell, ZwlrLayerShellV1);
proto_get!(shm, WlShm);
proto_get!(output_capture, ExtOutputImageCaptureSourceManagerV1);
proto_get!(image_copy, ExtImageCopyCaptureManagerV1);
proto_get!(xdg_output, ZxdgOutputManagerV1);
proto_get!(shape_manager, WpCursorShapeManagerV1);
