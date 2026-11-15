/// 3D Axis-Aligned Bounding Box (AABB).
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(C)]
pub struct Aabb3D {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Aabb3D {
    #[must_use]
    pub const fn new(min: [f32; 3], max: [f32; 3]) -> Self {
        Self { min, max }
    }

    #[must_use]
    pub fn from_points(points: &[[f32; 3]]) -> Self {
        if points.is_empty() {
            return Self::new([0.0; 3], [0.0; 3]);
        }
        let mut min = points[0];
        let mut max = points[0];
        for p in &points[1..] {
            min[0] = min[0].min(p[0]);
            min[1] = min[1].min(p[1]);
            min[2] = min[2].min(p[2]);

            max[0] = max[0].max(p[0]);
            max[1] = max[1].max(p[1]);
            max[2] = max[2].max(p[2]);
        }
        Self::new(min, max)
    }

    #[must_use]
    pub fn center(&self) -> [f32; 3] {
        [
            f32::midpoint(self.min[0], self.max[0]),
            f32::midpoint(self.min[1], self.max[1]),
            f32::midpoint(self.min[2], self.max[2]),
        ]
    }

    #[must_use]
    pub fn extents(&self) -> [f32; 3] {
        [
            (self.max[0] - self.min[0]) * 0.5,
            (self.max[1] - self.min[1]) * 0.5,
            (self.max[2] - self.min[2]) * 0.5,
        ]
    }

    #[must_use]
    pub fn bounding_sphere(&self) -> BoundingSphere {
        let c = self.center();
        let e = self.extents();
        let r = (e[0] * e[0] + e[1] * e[1] + e[2] * e[2]).sqrt();
        BoundingSphere::new(c, r)
    }

    #[must_use]
    pub fn contains_point(&self, p: [f32; 3]) -> bool {
        p[0] >= self.min[0]
            && p[0] <= self.max[0]
            && p[1] >= self.min[1]
            && p[1] <= self.max[1]
            && p[2] >= self.min[2]
            && p[2] <= self.max[2]
    }

    #[must_use]
    pub fn intersects_aabb(&self, other: &Self) -> bool {
        self.min[0] <= other.max[0]
            && self.max[0] >= other.min[0]
            && self.min[1] <= other.max[1]
            && self.max[1] >= other.min[1]
            && self.min[2] <= other.max[2]
            && self.max[2] >= other.min[2]
    }
}

/// 3D Bounding Sphere.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(C)]
pub struct BoundingSphere {
    pub center: [f32; 3],
    pub radius: f32,
}

impl BoundingSphere {
    #[must_use]
    pub const fn new(center: [f32; 3], radius: f32) -> Self {
        Self { center, radius }
    }

    #[must_use]
    pub fn contains_point(&self, p: [f32; 3]) -> bool {
        let dx = p[0] - self.center[0];
        let dy = p[1] - self.center[1];
        let dz = p[2] - self.center[2];
        (dx * dx + dy * dy + dz * dz) <= (self.radius * self.radius)
    }

    #[must_use]
    pub fn intersects_sphere(&self, other: &Self) -> bool {
        let dx = other.center[0] - self.center[0];
        let dy = other.center[1] - self.center[1];
        let dz = other.center[2] - self.center[2];
        let dist_sq = dx * dx + dy * dy + dz * dz;
        let r_sum = self.radius + other.radius;
        dist_sq <= (r_sum * r_sum)
    }
}
