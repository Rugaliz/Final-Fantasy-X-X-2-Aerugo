//! Skeletons of skinned meshes: the joint hierarchy with its default pose, and the skeleton (inverse bind)
//! matrices that segments' skin indices refer to.

use crate::mesh::{IDENTITY, Segment, mul};
use crate::phyre::{Cluster, ObjectId};

#[derive(Debug, Clone, PartialEq)]
pub struct Skeleton {
    pub names: Vec<String>,
    /// Parent joint, or -1 for roots.
    pub parents: Vec<i32>,
    /// Column-major local matrices of the default pose (relative to the parent).
    pub default_pose: Vec<[f32; 16]>,
    /// Column-major skeleton matrices: model space → joint space at bind time.
    pub inverse_bind: Vec<[f32; 16]>,
}

fn matrices(cluster: &Cluster, objs: Vec<ObjectId>) -> Vec<[f32; 16]> {
    objs.into_iter()
        .map(|m| cluster.floats(m, "m_elements", 16).and_then(|f| f.try_into().ok()).unwrap_or(IDENTITY))
        .collect()
}

/// The skeleton of a `PMesh`, if it has joints.
pub fn read_skeleton(cluster: &Cluster, mesh: ObjectId) -> Option<Skeleton> {
    let default_pose = matrices(cluster, cluster.array_targets(mesh, "m_defaultPose"));
    if default_pose.is_empty() {
        return None;
    }
    let inverse_bind = matrices(cluster, cluster.array_targets(mesh, "m_skeletonMatrices"));
    let names = cluster
        .array_targets(mesh, "m_matrixNames")
        .into_iter()
        .map(|s| cluster.string(s, "m_buffer").unwrap_or_default())
        .collect();
    let parents = match cluster.array(mesh, "m_matrixParents", 4) {
        Some(a) => {
            let n = default_pose.len();
            let bytes = cluster.array_bytes(mesh, a, n * 4);
            bytes.chunks_exact(4).map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
        }
        None => vec![-1; default_pose.len()],
    };
    Some(Skeleton { names, parents, default_pose, inverse_bind })
}

impl Skeleton {
    pub fn len(&self) -> usize {
        self.default_pose.len()
    }

    pub fn is_empty(&self) -> bool {
        self.default_pose.is_empty()
    }

    /// World (model-space) matrices from local ones. Parents may come after children in the list.
    pub fn world(&self, local: &[[f32; 16]]) -> Vec<[f32; 16]> {
        let n = local.len();
        let mut world: Vec<Option<[f32; 16]>> = vec![None; n];
        fn resolve(
            i: usize,
            sk: &Skeleton,
            local: &[[f32; 16]],
            world: &mut Vec<Option<[f32; 16]>>,
            depth: usize,
        ) -> [f32; 16] {
            if let Some(w) = world[i] {
                return w;
            }
            let p = sk.parents.get(i).copied().unwrap_or(-1);
            let w = if p >= 0 && (p as usize) < local.len() && p as usize != i && depth < 512 {
                let pw = resolve(p as usize, sk, local, world, depth + 1);
                mul(&pw, &local[i])
            } else {
                local[i]
            };
            world[i] = Some(w);
            w
        }
        (0..n).map(|i| resolve(i, self, local, &mut world, 0)).collect()
    }

    /// The joint palette of a segment: for each skin index, world(hierarchy joint) × skeleton matrix.
    pub fn palette(&self, world: &[[f32; 16]], segment: &Segment) -> Vec<[f32; 16]> {
        segment
            .skin_bones
            .iter()
            .map(|&(h, s)| {
                let w = world.get(h as usize).copied().unwrap_or(IDENTITY);
                let ib = self.inverse_bind.get(s as usize).copied().unwrap_or(IDENTITY);
                mul(&w, &ib)
            })
            .collect()
    }
}

/// Largest absolute difference from the identity.
pub fn identity_error(m: &[f32; 16]) -> f32 {
    m.iter().zip(IDENTITY.iter()).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max)
}
