use std::sync::Arc;

use roaring::RoaringBitmap;
use tej_db_core::spatial::{Aabb3D, BoundingSphere, MESHLET_HEADER_SIZE, MeshletPageHeader};
use tej_db_core::{PAGE_SIZE, Page, PageId, PageType, Result, Storage};
use tej_db_txn::TxManager;
pub use tej_db_vectorized::spatial_culling::{FrustumPlanes, Ray3D};

use crate::spatial_soa::SpatialSoACache;

#[cfg(test)]
#[path = "spatial_tests.rs"]
mod spatial_tests;

pub use crate::spatial_cluster::{ClusterMetadata, encode_composite_cluster_key};

/// Differential visibility results returned after each camera movement evaluation.
#[derive(Debug, Clone, PartialEq)]
pub struct CullResult {
    /// Full set of cluster IDs currently visible within the camera frustum and LOD threshold.
    pub visible_clusters: RoaringBitmap,
    /// Differential delta: cluster IDs newly visible that must be uploaded to GPU VRAM.
    pub to_upload: Vec<u32>,
    /// Differential delta: cluster IDs no longer visible that may be evicted from GPU VRAM.
    pub to_evict: Vec<u32>,
}

/// Scope filter for raycast picking operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaycastScope {
    /// Only test clusters currently resident in GPU visibility.
    VisibleOnly,
    /// Test all registered clusters in the spatial domain.
    AllClusters,
}

/// Represents a 3D translation applied to a cluster by an editor gizmo.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GizmoTransform {
    pub cluster_id: u32,
    pub translation: [f32; 3],
}

/// High-level 3D spatial viewport manager coordinating Nanite-style LOD clustering,
/// frustum culling, zero-copy GPU staging uploads, raycast picking, and ACID undo/redo.
pub struct SpatialViewportManager<S: Storage> {
    tx_manager: Arc<TxManager<S>>,
    table_name: String,
    domain: Aabb3D,
    clusters: Vec<ClusterMetadata>,
    gpu_resident: RoaringBitmap,
    undo_stack: Vec<GizmoTransform>,
    redo_stack: Vec<GizmoTransform>,
    soa: SpatialSoACache,
    visible_id_scratch: Vec<u32>,
    visible_cluster_indices: Vec<usize>,
}

impl<S: Storage> std::fmt::Debug for SpatialViewportManager<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpatialViewportManager")
            .field("table_name", &self.table_name)
            .field("domain", &self.domain)
            .field("cluster_count", &self.clusters.len())
            .field("gpu_resident_count", &self.gpu_resident.len())
            .finish_non_exhaustive()
    }
}

impl<S: Storage> SpatialViewportManager<S> {
    /// Initializes a new spatial viewport manager backed by `table_name`.
    pub fn new(
        tx_manager: Arc<TxManager<S>>,
        table_name: impl Into<String>,
        domain: Aabb3D,
    ) -> Result<Self> {
        let table_name = table_name.into();
        {
            let mut tx = tx_manager.begin_write()?;
            let _ = tx.create_table(&table_name);
            tx.commit()?;
        }

        let mut clusters = Vec::new();
        let mut soa = SpatialSoACache::new();

        if let Ok(rx) = tx_manager.begin_read() {
            if let Ok(tbl) = rx.open_table(&table_name) {
                let _ = tbl.scan_range_visitor(&tej_db_core::KeyRange::all(), |_k, v| {
                    if let Ok(cluster) = ClusterMetadata::from_bytes(v) {
                        soa.push(
                            cluster.sphere.center,
                            cluster.sphere.radius,
                            cluster.lod_error,
                            cluster.cluster_id,
                        );
                        clusters.push(cluster);
                    }
                    Ok(true)
                });
            }
        }

        Ok(Self {
            tx_manager,
            table_name,
            domain,
            clusters,
            gpu_resident: RoaringBitmap::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            soa,
            visible_id_scratch: Vec::new(),
            visible_cluster_indices: Vec::new(),
        })
    }

    /// Number of registered clusters.
    #[must_use]
    pub fn cluster_count(&self) -> usize {
        self.clusters.len()
    }

    /// Looks up a cluster metadata by its cluster ID.
    #[must_use]
    pub fn get_cluster(&self, cluster_id: u32) -> Option<&ClusterMetadata> {
        self.clusters.iter().find(|c| c.cluster_id == cluster_id)
    }

    /// Total bounding domain for Morton quantization.
    #[must_use]
    pub const fn domain(&self) -> Aabb3D {
        self.domain
    }

    /// Bitmap of cluster IDs currently marked resident on the GPU.
    #[must_use]
    pub const fn gpu_resident(&self) -> &RoaringBitmap {
        &self.gpu_resident
    }

    /// Registers a spatial cluster metadata entry, writing its composite Morton key index into the database table.
    pub fn register_cluster(&mut self, cluster: ClusterMetadata) -> Result<()> {
        let key_bytes = encode_composite_cluster_key(
            &self.domain,
            cluster.lod_level,
            cluster.sphere.center,
            cluster.cluster_id,
        );
        let val_bytes = cluster.to_bytes();

        let mut tx = self.tx_manager.begin_write()?;
        {
            let mut tbl = tx.open_table_mut(&self.table_name)?;
            tbl.put(&key_bytes, &val_bytes)?;
        }
        tx.commit()?;

        self.soa.push(
            cluster.sphere.center,
            cluster.sphere.radius,
            cluster.lod_error,
            cluster.cluster_id,
        );
        self.clusters.push(cluster);
        Ok(())
    }

    /// Writes a 64-byte aligned meshlet header and raw vertex/index payload into a dedicated 4 KiB page,
    /// returning the allocated `PageId` and registering the cluster.
    pub fn store_meshlet_page(
        &mut self,
        cluster_id: u32,
        header: &MeshletPageHeader,
        vertex_indices: &[u8],
    ) -> Result<PageId> {
        let page_id = self.tx_manager.cache().allocate_page()?;
        let mut page = Page::new(page_id, PageType::MeshletNode);
        let header_bytes = header.to_bytes();
        page.data[64..64 + MESHLET_HEADER_SIZE].copy_from_slice(&header_bytes);

        let max_payload = PAGE_SIZE - (64 + MESHLET_HEADER_SIZE);
        let payload_len = vertex_indices.len().min(max_payload);
        page.data[128..128 + payload_len].copy_from_slice(&vertex_indices[..payload_len]);
        page.update_checksum();

        self.tx_manager.cache().write_page(page)?;

        let metadata = ClusterMetadata::new(
            cluster_id,
            Aabb3D::new(header.min_bounds, header.max_bounds),
            BoundingSphere::new(
                [
                    header.bounding_sphere[0],
                    header.bounding_sphere[1],
                    header.bounding_sphere[2],
                ],
                header.bounding_sphere[3],
            ),
            header.lod_level,
            header.lod_error,
            page_id,
        );
        self.register_cluster(metadata)?;

        Ok(page_id)
    }

    /// Evaluates camera frustum culling and screen-space projected LOD error,
    /// returning the visible set and differential GPU upload/eviction deltas.
    pub fn on_camera_move(
        &mut self,
        frustum: &FrustumPlanes,
        camera_pos: [f32; 3],
        max_screen_error: f32,
    ) -> CullResult {
        self.soa.evaluate_cull(
            frustum,
            camera_pos,
            max_screen_error,
            &mut self.visible_id_scratch,
            &mut self.visible_cluster_indices,
        );

        let visible = RoaringBitmap::from_sorted_iter(self.visible_id_scratch.iter().copied())
            .unwrap_or_else(|_| RoaringBitmap::from_iter(self.visible_id_scratch.iter().copied()));

        if visible == self.gpu_resident {
            return CullResult {
                visible_clusters: visible,
                to_upload: Vec::new(),
                to_evict: Vec::new(),
            };
        }

        let to_upload: Vec<u32> = (&visible - &self.gpu_resident).into_iter().collect();
        let to_evict: Vec<u32> = (&self.gpu_resident - &visible).into_iter().collect();

        self.gpu_resident.clone_from(&visible);

        CullResult {
            visible_clusters: visible,
            to_upload,
            to_evict,
        }
    }

    /// Evaluates SIMD frustum and LOD culling across all registered clusters in cache,
    /// returning the number of visible clusters with zero heap allocation.
    #[inline]
    #[must_use]
    pub fn cull_frustum_simd(
        &self,
        frustum: &FrustumPlanes,
        camera_pos: [f32; 3],
        max_screen_error: f32,
    ) -> usize {
        self.soa
            .cull_frustum_simd(frustum, camera_pos, max_screen_error)
    }

    /// Accesses a cluster's 4 KiB page slice directly without heap allocation.
    pub fn with_meshlet_bytes<F, R>(&self, page_id: PageId, f: F) -> Result<R>
    where
        F: FnOnce(&[u8]) -> R,
    {
        let page = self.tx_manager.cache().get_page(page_id)?;
        Ok(f(page.as_slice()))
    }

    /// Performs high-speed branch-free raycast picking over visible or all registered clusters.
    /// Returns `Some((cluster_id, hit_distance))` for the nearest intersected bounding box.
    #[must_use]
    pub fn pick_ray(&self, ray: &Ray3D, scope: RaycastScope) -> Option<(u32, f32)> {
        let mut closest_hit: Option<(u32, f32)> = None;
        let mut best_dist = f32::INFINITY;

        let total_clusters = self.clusters.len();
        let use_visible =
            scope == RaycastScope::VisibleOnly && !self.visible_cluster_indices.is_empty();
        let count = if use_visible {
            self.visible_cluster_indices.len()
        } else {
            total_clusters
        };

        for i in 0..count {
            let idx = if use_visible {
                self.visible_cluster_indices[i]
            } else {
                i
            };
            let cluster = &self.clusters[idx];

            let oc_x = cluster.sphere.center[0] - ray.origin[0];
            let oc_y = cluster.sphere.center[1] - ray.origin[1];
            let oc_z = cluster.sphere.center[2] - ray.origin[2];
            let tca = oc_x * ray.dir[0] + oc_y * ray.dir[1] + oc_z * ray.dir[2];
            let r_sq = cluster.sphere.radius * cluster.sphere.radius;
            let oc_len_sq = oc_x * oc_x + oc_y * oc_y + oc_z * oc_z;

            if tca < 0.0 {
                if oc_len_sq > r_sq {
                    continue;
                }
            } else {
                let d2 = oc_len_sq - tca * tca;
                if d2 > r_sq {
                    continue;
                }
            }

            if let Some(dist) = ray.intersect_aabb(cluster.bounds.min, cluster.bounds.max) {
                if dist < best_dist {
                    best_dist = dist;
                    closest_hit = Some((cluster.cluster_id, dist));
                }
            }
        }

        closest_hit
    }

    /// Applies a 3D gizmo translation to a cluster, updating its spatial bounds and Morton key.
    /// Pushes the operation to the undo stack.
    pub fn apply_transform(&mut self, transform: GizmoTransform) -> Result<bool> {
        let Some(cluster) = self
            .clusters
            .iter_mut()
            .find(|c| c.cluster_id == transform.cluster_id)
        else {
            return Ok(false);
        };

        // Old key deletion
        let old_key = encode_composite_cluster_key(
            &self.domain,
            cluster.lod_level,
            cluster.sphere.center,
            cluster.cluster_id,
        );

        // Update bounds & sphere
        for i in 0..3 {
            cluster.bounds.min[i] += transform.translation[i];
            cluster.bounds.max[i] += transform.translation[i];
            cluster.sphere.center[i] += transform.translation[i];
        }

        // New key insertion
        let new_key = encode_composite_cluster_key(
            &self.domain,
            cluster.lod_level,
            cluster.sphere.center,
            cluster.cluster_id,
        );
        let val_bytes = cluster.to_bytes();

        self.soa
            .update_position(transform.cluster_id, cluster.sphere.center);

        let mut tx = self.tx_manager.begin_write()?;
        {
            let mut tbl = tx.open_table_mut(&self.table_name)?;
            tbl.delete(&old_key)?;
            tbl.put(&new_key, &val_bytes)?;
        }
        tx.commit()?;

        self.undo_stack.push(transform);
        self.redo_stack.clear();

        Ok(true)
    }

    /// Reverts the most recent gizmo transform, updating geometry bounds and B+ Tree keys.
    pub fn undo(&mut self) -> Result<Option<GizmoTransform>> {
        let Some(transform) = self.undo_stack.pop() else {
            return Ok(None);
        };

        let Some(cluster) = self
            .clusters
            .iter_mut()
            .find(|c| c.cluster_id == transform.cluster_id)
        else {
            return Ok(None);
        };

        let old_key = encode_composite_cluster_key(
            &self.domain,
            cluster.lod_level,
            cluster.sphere.center,
            cluster.cluster_id,
        );

        // Invert translation
        for i in 0..3 {
            cluster.bounds.min[i] -= transform.translation[i];
            cluster.bounds.max[i] -= transform.translation[i];
            cluster.sphere.center[i] -= transform.translation[i];
        }

        let new_key = encode_composite_cluster_key(
            &self.domain,
            cluster.lod_level,
            cluster.sphere.center,
            cluster.cluster_id,
        );
        let val_bytes = cluster.to_bytes();

        self.soa
            .update_position(transform.cluster_id, cluster.sphere.center);

        let mut tx = self.tx_manager.begin_write()?;
        {
            let mut tbl = tx.open_table_mut(&self.table_name)?;
            tbl.delete(&old_key)?;
            tbl.put(&new_key, &val_bytes)?;
        }
        tx.commit()?;

        self.redo_stack.push(transform);
        Ok(Some(transform))
    }

    /// Redoes the most recently undone gizmo transform.
    pub fn redo(&mut self) -> Result<Option<GizmoTransform>> {
        let Some(transform) = self.redo_stack.pop() else {
            return Ok(None);
        };

        let Some(cluster) = self
            .clusters
            .iter_mut()
            .find(|c| c.cluster_id == transform.cluster_id)
        else {
            return Ok(None);
        };

        let old_key = encode_composite_cluster_key(
            &self.domain,
            cluster.lod_level,
            cluster.sphere.center,
            cluster.cluster_id,
        );

        for i in 0..3 {
            cluster.bounds.min[i] += transform.translation[i];
            cluster.bounds.max[i] += transform.translation[i];
            cluster.sphere.center[i] += transform.translation[i];
        }

        let new_key = encode_composite_cluster_key(
            &self.domain,
            cluster.lod_level,
            cluster.sphere.center,
            cluster.cluster_id,
        );
        let val_bytes = cluster.to_bytes();

        self.soa
            .update_position(transform.cluster_id, cluster.sphere.center);

        let mut tx = self.tx_manager.begin_write()?;
        {
            let mut tbl = tx.open_table_mut(&self.table_name)?;
            tbl.delete(&old_key)?;
            tbl.put(&new_key, &val_bytes)?;
        }
        tx.commit()?;

        self.undo_stack.push(transform);
        Ok(Some(transform))
    }
}
