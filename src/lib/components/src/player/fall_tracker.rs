use bevy_ecs::prelude::Component;

/// The height a player fell from, used to charge the landing.
///
/// While a player is in the air the tracker holds the highest point they reached, so the damage on
/// landing is measured from the top of the jump and not from the block they left. Once they touch
/// the ground the tracker resets to the landing height.
#[derive(Component, Debug, Clone, Copy)]
pub struct FallTracker {
    highest_y: f64,
}

impl FallTracker {
    /// A tracker that starts on the ground at the given height.
    pub fn on_ground(y: f64) -> Self {
        Self { highest_y: y }
    }

    /// A tracker with nothing recorded yet.
    pub fn new() -> Self {
        Self::on_ground(0.0)
    }

    /// Records a position while the player is in the air.
    pub fn track_airborne(&mut self, y: f64) {
        if y > self.highest_y {
            self.highest_y = y;
        }
    }

    /// Clears the record and returns how far the player fell.
    ///
    /// The distance is measured from the highest point reached to the landing height.
    pub fn land(&mut self, y: f64) -> f64 {
        let distance = self.highest_y - y;
        self.highest_y = y;
        distance.max(0.0)
    }
}

impl Default for FallTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falling_from_a_height_reports_the_distance() {
        let mut tracker = FallTracker::on_ground(70.0);
        tracker.track_airborne(70.0);
        tracker.track_airborne(75.0);
        tracker.track_airborne(73.0);
        assert_eq!(tracker.land(64.0), 11.0);
    }

    #[test]
    fn walking_on_flat_ground_reports_no_distance() {
        let mut tracker = FallTracker::on_ground(64.0);
        tracker.track_airborne(64.0);
        assert_eq!(tracker.land(64.0), 0.0);
    }

    #[test]
    fn landing_resets_the_record() {
        let mut tracker = FallTracker::new();
        tracker.track_airborne(90.0);
        tracker.land(60.0);
        assert_eq!(tracker.land(60.0), 0.0);
    }

    #[test]
    fn a_fall_upwards_never_reports_negative_distance() {
        let mut tracker = FallTracker::on_ground(60.0);
        assert_eq!(tracker.land(70.0), 0.0);
    }
}
