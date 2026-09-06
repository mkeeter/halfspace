//! Cache for WGPU textures, indexed by data and size
use eframe::egui_wgpu::wgpu;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

/// Helper type which compares and sorts `Arc<T>` only by pointer
///
/// It also owns the `Arc`, so we don't need to worry about allocation reuse
struct ArcKey<T: ?Sized>(Arc<T>);

impl<T: ?Sized> Clone for ArcKey<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T: ?Sized> std::cmp::PartialEq for ArcKey<T> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(Arc::as_ptr(&self.0), Arc::as_ptr(&other.0))
    }
}
impl<T: ?Sized> std::cmp::Eq for ArcKey<T> {}
impl<T: ?Sized> std::hash::Hash for ArcKey<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state)
    }
}
impl<T: ?Sized> std::cmp::PartialOrd for ArcKey<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<T: ?Sized> std::cmp::Ord for ArcKey<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        Arc::as_ptr(&self.0)
            .cast::<()>()
            .cmp(&Arc::as_ptr(&other.0).cast::<()>())
    }
}

pub struct WgpuTextureCache<T: ?Sized> {
    ptr_to_texture: HashMap<ArcKey<T>, wgpu::Texture>,
    size_to_texture: BTreeMap<([u32; 3], Option<ArcKey<T>>), wgpu::Texture>,
}

pub enum CacheHit {
    DataMatch(wgpu::Texture),
    SizeMatch(wgpu::Texture),
}

impl<T: ?Sized> WgpuTextureCache<T> {
    pub fn new() -> Self {
        Self {
            ptr_to_texture: HashMap::new(),
            size_to_texture: BTreeMap::new(),
        }
    }

    /// Gets a texture with matching data or size, if present
    pub fn get(
        &mut self,
        data: &Arc<T>,
        size: wgpu::Extent3d,
    ) -> Option<CacheHit> {
        let k = ArcKey(data.clone());
        if let Some(d) = self.ptr_to_texture.remove(&k) {
            let tex_size = d.size();
            assert_eq!(tex_size, size);
            self.size_to_texture
                .remove(&(
                    [size.width, size.height, size.depth_or_array_layers],
                    Some(k),
                ))
                .unwrap();
            return Some(CacheHit::DataMatch(d));
        }

        let size = [size.width, size.height, size.depth_or_array_layers];
        let mut iter = self.size_to_texture.range((size, None)..);
        let ((tex_size, ptr), tex) = iter.next()?;
        if *tex_size != size {
            return None;
        }
        let ptr = ptr.as_ref().unwrap().clone();
        let tex = tex.clone();

        self.ptr_to_texture.remove(&ptr).unwrap();
        self.size_to_texture.remove(&(size, Some(ptr))).unwrap();

        Some(CacheHit::SizeMatch(tex))
    }

    pub fn clear(&mut self) {
        // TODO add some kind of cache aging instead?
        self.ptr_to_texture.clear();
        self.size_to_texture.clear();
    }

    pub fn insert(&mut self, data: Arc<T>, tex: wgpu::Texture) {
        let prev = self
            .ptr_to_texture
            .insert(ArcKey(data.clone()), tex.clone());
        assert!(prev.is_none());

        let size = tex.size();
        let prev = self.size_to_texture.insert(
            (
                [size.width, size.height, size.depth_or_array_layers],
                Some(ArcKey(data.clone())),
            ),
            tex,
        );
        assert!(prev.is_none());
    }
}
