extern crate gfx;
extern crate gfx_device_gl;

use self::gfx::traits::FactoryExt;
use renderer::VertexPosNormal;
use asset_manager::{AssetLoader, Assets};
use gfx_device::gfx_types;

#[derive(Clone)]
/// This struct represents a piece of geometry. It is part of a `Renderable`
pub struct Mesh {
    pub buffer: gfx::handle::Buffer<gfx_types::Resources, VertexPosNormal>,
    pub slice: gfx::Slice<gfx_types::Resources>,
}

impl AssetLoader<Mesh> for Vec<VertexPosNormal> {
    /// # Panics
    /// Panics if factory isn't registered as loader.
    fn from_data(assets: &mut Assets, data: Vec<VertexPosNormal>) -> Option<Mesh> {
        let factory = assets.get_loader_mut::<gfx_types::Factory>().expect("Couldn't retrieve factory.");
        let (buffer, slice) = factory.create_vertex_buffer_with_slice(&data, ());
        Some(Mesh {
            buffer: buffer,
            slice: slice,
        })
    }
}
