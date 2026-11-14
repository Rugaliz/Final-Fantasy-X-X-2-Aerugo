//! Cameras stored in Phyre clusters (`PCameraPerspective`, `PCameraOrthographic`). 2D-background maps carry
//! the camera their layers were made for.

use crate::mesh::{IDENTITY, node_world};
use crate::phyre::Cluster;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Projection {
    /// Vertical field of view in radians.
    Perspective { fov_y: f32 },
    /// Height of the view volume.
    Orthographic { height: f32 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Camera {
    pub name: String,
    pub projection: Projection,
    pub near: f32,
    pub far: f32,
    /// Column-major camera-to-world transform (the camera looks down its −Z axis).
    pub world: [f32; 16],
}

pub fn read_cameras(cluster: &Cluster) -> Vec<Camera> {
    let mut out = Vec::new();
    for cam in cluster.objects("PCamera") {
        let class = cluster.class_name(cam).to_owned();
        let projection = if class == "PCameraPerspective" {
            Projection::Perspective { fov_y: cluster.f32(cam, "m_FOV").unwrap_or(0.8) }
        } else if class == "PCameraOrthographic" {
            Projection::Orthographic { height: cluster.f32(cam, "m_height").unwrap_or(1.0) }
        } else {
            continue;
        };
        let world = cluster
            .target(cam, "m_localToWorldMatrix")
            .and_then(|wm| {
                cluster.objects("PNode").into_iter().find(|&n| cluster.target(n, "m_worldMatrix") == Some(wm))
            })
            .map(|n| node_world(cluster, n))
            .unwrap_or(IDENTITY);
        out.push(Camera {
            name: cluster.string(cam, "m_name").unwrap_or_default(),
            projection,
            near: cluster.f32(cam, "m_nearPlane").unwrap_or(0.1),
            far: cluster.f32(cam, "m_farPlane").unwrap_or(10_000.0),
            world,
        });
    }
    out
}
