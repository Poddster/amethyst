//! This module provides an asset manager
//! which loads and provides access to assets,
//! such as `Texture`s, `Mesh`es, and `Fragment`s.

extern crate amethyst_ecs;
extern crate amethyst_renderer;
extern crate cgmath;
extern crate genmesh;
extern crate gfx_device_gl;
extern crate gfx;
extern crate imagefmt;
extern crate wavefront_obj;


// stdlib imports
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{Cursor, Read};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::str;
use std::sync::RwLockReadGuard;

// self imports
use components::rendering::{Mesh, Renderable, Texture, TextureLoadData};
use renderer::VertexPosNormal;

// external imports
use self::amethyst_ecs::{Allocator, Component, Entity, MaskedStorage, Storage, VecStorage, World};
use self::cgmath::{InnerSpace, Vector3};
pub use self::gfx::tex::{AaMode, Kind};
use self::wavefront_obj::obj::{ObjSet, parse, Primitive};


type AssetTypeId = TypeId;
type SourceTypeId = TypeId;
type LoaderTypeId = TypeId;

/// Id for directly accessing assets in the manager
pub type AssetId = Entity;

/// Wrapper type for actual asset data
pub struct Asset<T>(pub T);

impl<T: Any + Send + Sync> Component for Asset<T> {
    type Storage = VecStorage<Asset<T>>;
}

/// A trait for generating intermdiate data for loading from raw data
pub trait AssetLoaderRaw: Sized {
    fn from_raw(assets: &Assets, data: &[u8]) -> Option<Self>;
}

/// A trait for loading assets from arbitrary data
pub trait AssetLoader<A> {
    // TODO: Return Ok instead of Option
    fn from_data(assets: &mut Assets, data: Self) -> Option<A>;
}

/// A trait for asset stores which are permanent storages for assets
pub trait AssetStore {
    fn has_asset(&self, name: &str, asset_type: &str) -> bool;
    fn load_asset(&self, name: &str, asset_type: &str, buf: &mut Vec<u8>) -> Option<usize>;
}

pub trait AssetReadStorage<T> {
    fn read(&self, id: AssetId) -> Option<&T>;
}

impl<'a, T: Any + Send + Sync> AssetReadStorage<T> for Storage<Asset<T>, RwLockReadGuard<'a, Allocator>, RwLockReadGuard<'a, MaskedStorage<Asset<T>>>> {
    fn read(&self, id: AssetId) -> Option<&T> {
        self.get(id).map(|asset| &asset.0)
    }
}

/// Internal assets handler which takes care of storing and loading assets.
pub struct Assets {
    loaders: HashMap<LoaderTypeId, Box<Any>>,
    asset_ids: HashMap<String, AssetId>,
    assets: World,
}

impl Assets {
    fn new() -> Assets {
        Assets {
            loaders: HashMap::new(),
            asset_ids: HashMap::new(),
            assets: World::new(),
        }
    }

    /// Add loader resource to the manager
    pub fn add_loader<T: Any>(&mut self, loader: T) {
        let loader = Box::new(loader);
        self.loaders.insert(TypeId::of::<T>(), loader);
    }

    /// Returns stored loader resource
    pub fn get_loader<T: Any>(&self) -> Option<&T> {
        self.loaders
            .get(&TypeId::of::<T>())
            .and_then(|loader| loader.downcast_ref())
    }

    // Returns stored loader resource
    pub fn get_loader_mut<T: Any>(&mut self) -> Option<&mut T> {
        self.loaders
            .get_mut(&TypeId::of::<T>())
            .and_then(|loader| loader.downcast_mut())
    }

    /// Register a new asset type
    pub fn register_asset<A: Any + Send + Sync>(&mut self) {
        self.assets.register::<Asset<A>>();
    }

    /// Retrieve the `AssetId` from the asset name
    pub fn id_from_name(&self, name: &str) -> Option<AssetId> {
        self.asset_ids.get(name).map(|id| *id)
    }

    /// Read the storage of all assets for a certain type
    pub fn read_assets<A: Any + Send + Sync>(&self) -> Storage<Asset<A>, RwLockReadGuard<Allocator>, RwLockReadGuard<MaskedStorage<Asset<A>>>> {
        self.assets.read()
    }

    /// Load an asset from data
    pub fn load_asset_from_data<A: Any + Sync + Send, S>(&mut self, name: &str, data: S) -> Option<AssetId>
        where S: AssetLoader<A>
    {
        let asset = AssetLoader::<A>::from_data(self, data);
        if let Some(asset) = asset {
            Some(self.add_asset(name, asset))
        } else {
            None
        }
    }

    fn add_asset<A: Any + Send + Sync>(&mut self, name: &str, asset: A) -> AssetId {
        *self.asset_ids.entry(name.into()).or_insert(self.assets.create_now().with(Asset::<A>(asset)).build())
    }
}

/// Asset manager which handles assets and loaders.
pub struct AssetManager {
    assets: Assets,
    asset_type_ids: HashMap<(String, AssetTypeId), SourceTypeId>,
    closures: HashMap<(AssetTypeId, SourceTypeId), Box<FnMut(&mut Assets, &str, &[u8]) -> Option<AssetId>>>,
    stores: Vec<Box<AssetStore>>,
}

impl AssetManager {
    /// Create a new asset manager
    pub fn new() -> AssetManager {
        let mut asset_manager = AssetManager {
            asset_type_ids: HashMap::new(),
            assets: Assets::new(),
            closures: HashMap::new(),
            stores: Vec::new(),
        };

        // Handle some common use cases by default
        asset_manager.register_asset::<Mesh>();
        asset_manager.register_asset::<Texture>();

        asset_manager.register_loader::<Mesh, ObjSet>("obj");

        for fmt in vec!["png", "bmp", "jpg", "jpeg", "tga"] {
            asset_manager.register_loader::<Texture, imagefmt::Image<u8>>(fmt);
        }

        // Set up default resource directories. Will add each dir in
        // `AMETHYST_ASSET_DIRS` if set. Will also add the current
        // executable's sibling `./resources/assets/` directory.
        if let Ok(paths) = env::var("AMETHYST_ASSET_DIRS") {
            for dir in env::split_paths(&paths) {
                asset_manager.register_store(DirectoryStore::new(dir));
            }
        }

        if let Ok(e) = env::current_exe() {
            if let Some(dir) = e.parent() {
                let current_dir = format!("{}/resources/assets", dir.display());
                asset_manager.register_store(DirectoryStore::new(current_dir));
            }
        }

        asset_manager
    }

    /// Register a new loading method for a specific asset data type
    pub fn register_loader<A: Any + Send + Sync, S: Any>(&mut self, asset: &str)
        where S: AssetLoader<A> + AssetLoaderRaw
    {
        let asset_id = TypeId::of::<A>();
        let source_id = TypeId::of::<S>();

        self.closures.insert((asset_id, source_id),
                             Box::new(|loader: &mut Assets, name: &str, raw: &[u8]| {
            S::from_raw(loader, raw)
                .and_then(|data| {
                    AssetLoader::<A>::from_data(loader, data)
                })
                .and_then(|asset| {
                    Some(loader.add_asset(name, asset))
                })
        }));

        self.asset_type_ids.insert((asset.into(), asset_id), source_id);
    }

    /// Register an asset store
    pub fn register_store<T: 'static + AssetStore>(&mut self, store: T) {
        self.stores.push(Box::new(store));
    }

    /// Load an asset from raw data
    /// # Panics
    /// Panics if the asset type isn't registered
    pub fn load_asset_from_raw<A: Any + Send + Sync>(&mut self, name: &str, asset_type: &str, raw: &[u8]) -> Option<AssetId> {
        let asset_type_id = TypeId::of::<A>();
        let &source_id = self.asset_type_ids.get(&(asset_type.into(), asset_type_id)).expect("Unregistered asset type id");
        let ref mut loader = self.closures.get_mut(&(asset_type_id, source_id)).unwrap();
        loader(&mut self.assets, name, raw)
    }

    /// Load an asset from the asset stores
    pub fn load_asset<A: Any + Send + Sync>(&mut self, name: &str, asset_type: &str) -> Option<AssetId> {
        let mut buf = Vec::new();
        if let Some(store) = self.stores.iter().find(|store| store.has_asset(name, asset_type)) {
            store.load_asset(name, asset_type, &mut buf);
        } else {
            return None;
        }

        self.load_asset_from_raw::<A>(name, asset_type, &buf)
    }

    /// Create a `Renderable` component from a loaded mesh and ka/kd/ks textures
    pub fn create_renderable(&self, mesh: &str, ka: &str, kd: &str, ks: &str, ns: f32) -> Option<Renderable> {
        let meshes = self.read_assets::<Mesh>();
        let textures = self.read_assets::<Texture>();
        let mesh_id = match self.id_from_name(mesh) {
            Some(id) => id,
            None => return None,
        };
        let mesh = match meshes.read(mesh_id) {
            Some(mesh) => mesh,
            None => return None,
        };
        let ka_id = match self.id_from_name(ka) {
            Some(id) => id,
            None => return None,
        };
        let ka = match textures.read(ka_id) {
            Some(ka) => ka,
            None => return None,
        };
        let kd_id = match self.id_from_name(kd) {
            Some(id) => id,
            None => return None,
        };
        let kd = match textures.read(kd_id) {
            Some(kd) => kd,
            None => return None,
        };
        let ks_id = match self.id_from_name(ks) {
            Some(id) => id,
            None => return None,
        };
        let ks = match textures.read(ks_id) {
            Some(ks) => ks,
            None => return None,
        };
        Some(Renderable {
            mesh: mesh.clone(),
            ambient: ka.clone(),
            diffuse: kd.clone(),
            specular: ks.clone(),
            specular_exponent: ns,
        })
    }
}

impl Deref for AssetManager {
    type Target = Assets;

    fn deref(&self) -> &Assets {
        &self.assets
    }
}

impl DerefMut for AssetManager {
    fn deref_mut(&mut self) -> &mut Assets {
        &mut self.assets
    }
}

/// Asset store representing a file directory.
pub struct DirectoryStore {
    path: PathBuf,
}

impl DirectoryStore {
    pub fn new<P: AsRef<Path>>(path: P) -> DirectoryStore {
        DirectoryStore { path: path.as_ref().to_path_buf() }
    }

    fn asset_to_path<'a>(&self, name: &str, asset_type: &str) -> PathBuf {
        let file_name = format!("{}.{}", name, asset_type);
        self.path.join(file_name)
    }
}

impl AssetStore for DirectoryStore {
    fn has_asset(&self, name: &str, asset_type: &str) -> bool {
        let file_path = self.asset_to_path(name, asset_type);
        fs::metadata(file_path).ok().map(|meta| meta.is_file()).is_some()
    }

    fn load_asset(&self, name: &str, asset_type: &str, buf: &mut Vec<u8>) -> Option<usize> {
        let file_path = self.asset_to_path(name, asset_type);
        let mut file = if let Ok(file) = fs::File::open(file_path) {
            file
        } else {
            return None;
        };
        file.read_to_end(buf).ok()
    }
}

impl AssetLoaderRaw for imagefmt::Image<u8> {
    fn from_raw(_: &Assets, data: &[u8]) -> Option<imagefmt::Image<u8>> {
        imagefmt::read_from(&mut Cursor::new(data), imagefmt::ColFmt::RGBA).ok()
    }
}

impl AssetLoader<Texture> for imagefmt::Image<u8> {
    fn from_data(assets: &mut Assets, image: imagefmt::Image<u8>) -> Option<Texture> {
        let pixels = image.buf.chunks(4).map(|p| [p[0], p[1], p[2], p[3]]).collect::<Vec<_>>();

        AssetLoader::from_data(assets, TextureLoadData {
            kind: Kind::D2(image.w as u16, image.h as u16, AaMode::Single),
            raw: &[pixels.as_slice()],
        })
    }
}

impl AssetLoaderRaw for ObjSet {
    fn from_raw(_: &Assets, data: &[u8]) -> Option<ObjSet> {
        if let Some(data) = str::from_utf8(data).ok() {
            parse(data.into()).ok()
        } else {
            None
        }
    }
}

impl AssetLoader<Mesh> for ObjSet {
    fn from_data(assets: &mut Assets, obj_set: ObjSet) -> Option<Mesh> {
        // Takes a list of objects that contain geometries that contain shapes that contain
        // vertex/texture/normal indices into the main list of vertices, and converts to a
        // flat vec of `VertexPosNormal` objects.
        // TODO: Doesn't differentiate between objects in a `*.obj` file, treats
        // them all as a single mesh.
        let vertices: Vec<VertexPosNormal> = obj_set.objects.iter().flat_map(|object| {
            object.geometry.iter().flat_map(|ref geometry| {
                geometry.shapes.iter().flat_map(|s| -> Vec<VertexPosNormal> {
                    let mut vtn_indices = vec![];

                    match s.primitive {
                        Primitive::Point(v1) => {
                            vtn_indices.push(v1);
                        },
                        Primitive::Line(v1, v2) => {
                            vtn_indices.push(v1);
                            vtn_indices.push(v2);
                        },
                        Primitive::Triangle(v1, v2, v3) => {
                            vtn_indices.push(v1);
                            vtn_indices.push(v2);
                            vtn_indices.push(v3);
                        },
                    }

                    vtn_indices.iter().map(|&(vi, ti, ni)| {
                        let vertex = object.vertices[vi];

                        VertexPosNormal {
                            pos: [
                                vertex.x as f32,
                                vertex.y as f32,
                                vertex.z as f32
                            ],
                            normal: match ni {
                                Some(i) => {
                                    let normal = object.normals[i];

                                    Vector3::from([
                                        normal.x as f32,
                                        normal.y as f32,
                                        normal.z as f32
                                    ])
                                    .normalize()
                                    .into()
                                },
                                None => [0.0, 0.0, 0.0],
                            },
                            tex_coord: match ti {
                                Some(i) => {
                                    let tvertex = object.tex_vertices[i];
                                    [tvertex.u as f32, tvertex.v as f32]
                                },
                                None => [0.0, 0.0],
                            },
                        }
                    }).collect()
                })
            }).collect::<Vec<VertexPosNormal>>()
        }).collect();

        AssetLoader::<Mesh>::from_data(assets, vertices)
    }
}

#[cfg(test)]
mod tests {
    use super::{Assets, AssetManager, AssetLoader, AssetLoaderRaw};

    #[derive(PartialEq, Debug)]
    struct Foo;
    struct FooLoader;

    impl AssetLoader<Foo> for u32 {
        fn from_data(_: &mut Assets, x: u32) -> Option<Foo> {
            if x == 10 { Some(Foo) } else { None }
        }
    }

    impl AssetLoaderRaw for u32 {
        fn from_raw(assets: &Assets, _: &[u8]) -> Option<u32> {
            let _ = assets.get_loader::<FooLoader>();
            Some(10)
        }
    }


    #[test]
    fn loader_resource() {
        let mut assets = AssetManager::new();
        assets.add_loader(0.0f32);
        assert_eq!(Some(&0.0f32), assets.get_loader::<f32>());
    }

    #[test]
    fn load_custom_asset() {
        let mut assets = AssetManager::new();
        assets.register_asset::<Foo>();
        assets.register_loader::<Foo, u32>("foo");
        assets.add_loader::<FooLoader>(FooLoader);

        assert!(assets.load_asset_from_raw::<Foo>("asset01", "foo", &[0; 2]).is_some());
        assert_eq!(None, assets.load_asset_from_data::<Foo, u32>("foo", 2));
    }

    #[test]
    fn load_duplicated_asset() {
        let mut assets = AssetManager::new();
        assets.register_asset::<Foo>();
        assets.register_loader::<Foo, u32>("foo");
        assets.add_loader::<FooLoader>(FooLoader);

        let asset01 = assets.load_asset_from_raw::<Foo>("asset01", "foo", &[0; 2]);
        assert_eq!(asset01, assets.load_asset_from_raw::<Foo>("asset01", "foo", &[0; 2]));
    }
}
