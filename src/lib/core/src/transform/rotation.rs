use bevy_ecs::prelude::Component;
use bitcode_derive::{Decode, Encode};
use std::fmt::Debug;
use typename::TypeName;

#[derive(TypeName, Component, Clone, Copy, Default, Decode, Encode)]
pub struct Rotation {
    pub yaw: f32,
    pub pitch: f32,
}

impl Rotation {
    pub fn new(yaw: f32, pitch: f32) -> Self {
        let yaw = yaw % 360.0; // Normalize yaw
        let pitch = pitch.clamp(-90.0, 90.0); // Clamp pitch
        Self { yaw, pitch }
    }

    pub fn rotate_yaw(&mut self, delta_yaw: f32) {
        self.yaw = (self.yaw + delta_yaw) % 360.0; // Normalize yaw
    }

    pub fn rotate_pitch(&mut self, delta_pitch: f32) {
        self.pitch = (self.pitch + delta_pitch).clamp(-90.0, 90.0); // Clamp pitch
    }

    pub fn to_radians(&self) -> (f32, f32) {
        (self.yaw.to_radians(), self.pitch.to_radians())
    }

    pub fn from_radians(yaw_radians: f32, pitch_radians: f32) -> Self {
        Self::new(yaw_radians.to_degrees(), pitch_radians.to_degrees())
    }

    /// The unit vector the entity is looking along.
    ///
    /// The axes follow Minecraft's convention, the same one `Entity.calculateViewVector` uses: yaw
    /// `0` looks towards `+Z` (south) and grows towards `-X` (west), while pitch is negative when
    /// looking up. Dropped items are thrown along this vector, so getting it wrong sends them
    /// sideways or into the ground instead of in front of the player.
    pub fn to_direction_vector(&self) -> (f32, f32, f32) {
        let yaw_rad = self.yaw.to_radians();
        let pitch_rad = self.pitch.to_radians();

        let x = -yaw_rad.sin() * pitch_rad.cos();
        let y = -pitch_rad.sin();
        let z = yaw_rad.cos() * pitch_rad.cos();

        (x, y, z)
    }

    /// Returns yaw and pitch as a tuple for easy spreading.
    ///
    /// This is useful for struct initialization where you need to spread
    /// rotation values across multiple fields.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let rot = Rotation::new(90.0, 45.0);
    /// let (yaw, pitch) = rot.yaw_pitch();
    /// // Now you can use yaw, pitch individually
    /// ```
    pub fn yaw_pitch(&self) -> (f32, f32) {
        (self.yaw, self.pitch)
    }
}

impl Debug for Rotation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Rotation {{ yaw: {:.2}°, pitch: {:.2}° (normalized) }}",
            self.yaw, self.pitch
        )
    }
}

impl From<(f32, f32)> for Rotation {
    fn from((yaw, pitch): (f32, f32)) -> Self {
        Self::new(yaw, pitch)
    }
}

impl From<Rotation> for (f32, f32) {
    fn from(rotation: Rotation) -> (f32, f32) {
        (rotation.yaw, rotation.pitch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compares a direction vector with a tolerance, since `sin`/`cos` of a right angle is never
    /// exactly zero in floating point.
    fn assert_direction(rotation: Rotation, expected: (f32, f32, f32)) {
        let (x, y, z) = rotation.to_direction_vector();
        let (ex, ey, ez) = expected;

        assert!(
            (x - ex).abs() < 1e-5 && (y - ey).abs() < 1e-5 && (z - ez).abs() < 1e-5,
            "yaw {} pitch {} gave ({x}, {y}, {z}), expected ({ex}, {ey}, {ez})",
            rotation.yaw,
            rotation.pitch
        );
    }

    #[test]
    fn yaw_zero_looks_towards_positive_z() {
        assert_direction(Rotation::new(0.0, 0.0), (0.0, 0.0, 1.0));
    }

    #[test]
    fn yaw_ninety_looks_towards_negative_x() {
        assert_direction(Rotation::new(90.0, 0.0), (-1.0, 0.0, 0.0));
    }

    #[test]
    fn yaw_one_eighty_looks_towards_negative_z() {
        assert_direction(Rotation::new(180.0, 0.0), (0.0, 0.0, -1.0));
    }

    #[test]
    fn negative_yaw_looks_towards_positive_x() {
        assert_direction(Rotation::new(-90.0, 0.0), (1.0, 0.0, 0.0));
    }

    #[test]
    fn looking_up_has_a_positive_y() {
        assert_direction(Rotation::new(0.0, -90.0), (0.0, 1.0, 0.0));
    }

    #[test]
    fn looking_down_has_a_negative_y() {
        assert_direction(Rotation::new(0.0, 90.0), (0.0, -1.0, 0.0));
    }
}
