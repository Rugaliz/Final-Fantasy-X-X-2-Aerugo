//! The `scene` block every effect reads (`$Globals.scene`): camera, viewport, ambient, fog, time, background.
//!
//! The remaster works in its own space, game space turned half round Z (x and y negated). Its view is a
//! right-handed look-at with y up; its projection a right-handed perspective with depth 0–1, near 1 and far
//! 65536, whose vertical field of view has half the frame height 240 at the script's screen distance;
//! `ViewProjection2` is the same perspective with OpenGL's depth range. (Checked value for value against the
//! original's constants in cdsp0700: View, Projection, ViewProjection, ViewProjection2, ViewInverse.)

use glam::{Mat4, Vec3};

use super::Value;

/// Game space → the remaster's space.
pub fn to_hd(p: Vec3) -> Vec3 {
    Vec3::new(-p.x, -p.y, p.z)
}

/// The scene constants of a frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneConstants {
    /// Eye in the remaster's space.
    pub eye: Vec3,
    pub view: Mat4,
    pub projection: Mat4,
    /// The same perspective with OpenGL's depth range.
    pub projection_gl: Mat4,
    pub near: f32,
    pub far: f32,
    pub viewport: [f32; 2],
    pub ambient: [f32; 3],
    pub fog_color: [f32; 3],
    pub fog_near: f32,
    pub fog_far: f32,
    pub fog_curve: f32,
    pub fog_limit: f32,
    pub time: f32,
    pub background: [f32; 3],
}

/// Near and far planes of the remaster's field cameras.
pub const NEAR: f32 = 1.0;
pub const FAR: f32 = 65536.0;

impl SceneConstants {
    /// Camera constants from a script camera in game space (eye, focus, screen distance) for a `width`×`height`
    /// frame. A screen distance of 0 means the default 1024.
    pub fn camera(eye: Vec3, focus: Vec3, screen_distance: f32, width: f32, height: f32) -> SceneConstants {
        let (eye, focus) = (to_hd(eye), to_hd(focus));
        let d = if screen_distance > 1.0 { screen_distance } else { 1024.0 };
        let fov_y = 2.0 * (240.0 / d).atan();
        let aspect = width / height.max(1.0);
        SceneConstants {
            eye,
            view: Mat4::look_at_rh(eye, focus, Vec3::Y),
            projection: Mat4::perspective_rh(fov_y, aspect, NEAR, FAR),
            projection_gl: Mat4::perspective_rh_gl(fov_y, aspect, NEAR, FAR),
            near: NEAR,
            far: FAR,
            viewport: [width, height],
            ambient: [0.0; 3],
            fog_color: [0.0; 3],
            fog_near: 0.0,
            fog_far: 0.0,
            fog_curve: 0.0,
            fog_limit: 0.0,
            time: 0.0,
            background: [0.0; 3],
        }
    }

    /// A `scene.<member>` value.
    pub fn value(&self, name: &str) -> Option<Value> {
        let m = |m: Mat4| Some(Value::Matrix(m));
        let f = |v: &[f32]| Some(Value::Floats(v.to_vec()));
        match name.strip_prefix("scene.")? {
            "EyePosition" => f(&self.eye.to_array()),
            "View" => m(self.view),
            "Projection" => m(self.projection),
            "ViewProjection" => m(self.projection * self.view),
            "ViewProjection2" => m(self.projection_gl),
            "ViewInverse" => m(self.view.inverse()),
            "cameraNearFar" => f(&[self.near, self.far]),
            "cameraNearTimesFar" => f(&[self.near * self.far]),
            "cameraFarMinusNear" => f(&[self.far - self.near]),
            "ViewportWidthHeight" => f(&self.viewport),
            "screenWidthHeightInv" => f(&[1.0 / self.viewport[0], 1.0 / self.viewport[1]]),
            "GlobalAmbientColor" => f(&self.ambient),
            "FogColor" => f(&self.fog_color),
            "FogNear" => f(&[self.fog_near]),
            "FogFar" => f(&[self.fog_far]),
            "FogCurve" => f(&[self.fog_curve]),
            "FogLimit" => f(&[self.fog_limit]),
            "GlobalTime" => f(&[self.time]),
            "BGColor" => f(&self.background),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// cdsp0700 as the original drew it (screen distance 562, a 2560×1440 frame): its captured constants.
    #[test]
    fn camera_matches_the_original() {
        let eye_hd = Vec3::new(-0.655_352, 73.2714, -182.591_32);
        let forward = Vec3::new(0.021_979, -0.108_04, 0.993_904);
        let s = SceneConstants::camera(to_hd(eye_hd), to_hd(eye_hd + forward * 100.0), 562.0, 2560.0, 1440.0);
        let close = |a: f32, b: f32| (a - b).abs() < 2e-3 * b.abs().max(1.0);
        // Projection rows (the original's registers): 1.317188, 2.341667, −1.000015 / −1.000015, −1.
        assert!(close(s.projection.col(0).x, 1.317_188), "{:?}", s.projection);
        assert!(close(s.projection.col(1).y, 2.341_667));
        assert!(close(s.projection.col(2).z, -1.000_015) && close(s.projection.col(3).z, -1.000_015));
        assert!(close(s.projection_gl.col(3).z, -2.000_031));
        // View: first row (−0.999756, 0, 0.022109, 3.381672), third row (−0.021979, 0.10804, −0.993904, −189.408844).
        let v = s.view;
        assert!(
            close(v.col(0).x, -0.999_756) && close(v.col(2).x, 0.022_109) && close(v.col(3).x, 3.381_672),
            "{v:?}"
        );
        assert!(close(v.col(1).z, 0.108_04) && close(v.col(3).z, -189.408_84), "{v:?}");
        assert!(close(v.col(3).y, -53.118_614), "{v:?}");
    }
}
