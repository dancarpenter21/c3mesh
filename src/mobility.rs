use crate::SimTime;
use serde::{Deserialize, Serialize};

/// A Cartesian position in meters.
///
/// Coordinates are intentionally reference-frame agnostic. A topology may use
/// local east/north/up coordinates, Earth-centered coordinates, or another
/// consistent frame appropriate to the simulation.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Position3D {
    /// X coordinate in meters.
    pub x_m: f64,
    /// Y coordinate in meters.
    pub y_m: f64,
    /// Z coordinate in meters.
    pub z_m: f64,
}

impl Position3D {
    /// The origin of a coordinate frame.
    pub const ORIGIN: Self = Self {
        x_m: 0.0,
        y_m: 0.0,
        z_m: 0.0,
    };

    /// Creates a position from coordinates measured in meters.
    #[must_use]
    pub const fn new(x_m: f64, y_m: f64, z_m: f64) -> Self {
        Self { x_m, y_m, z_m }
    }

    /// Returns the Euclidean distance to another position in meters.
    #[must_use]
    pub fn distance_to(self, other: Self) -> f64 {
        (self.x_m - other.x_m)
            .hypot(self.y_m - other.y_m)
            .hypot(self.z_m - other.z_m)
    }

    pub(crate) fn is_finite(self) -> bool {
        self.x_m.is_finite() && self.y_m.is_finite() && self.z_m.is_finite()
    }
}

/// A Cartesian velocity in meters per second.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Velocity3D {
    /// Velocity along the X axis in meters per second.
    pub x_mps: f64,
    /// Velocity along the Y axis in meters per second.
    pub y_mps: f64,
    /// Velocity along the Z axis in meters per second.
    pub z_mps: f64,
}

impl Velocity3D {
    /// Creates a velocity from Cartesian components.
    #[must_use]
    pub const fn new(x_mps: f64, y_mps: f64, z_mps: f64) -> Self {
        Self {
            x_mps,
            y_mps,
            z_mps,
        }
    }

    pub(crate) fn is_finite(self) -> bool {
        self.x_mps.is_finite() && self.y_mps.is_finite() && self.z_mps.is_finite()
    }
}

/// A position reached at a specific virtual time.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct Waypoint {
    /// Virtual time of the waypoint in nanoseconds.
    pub at_ns: u64,
    /// Position at that time.
    pub position: Position3D,
}

/// A deterministic description of a device's movement.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MobilityModel {
    /// A device that remains at one position.
    Static {
        /// Fixed position of the device.
        #[serde(default)]
        position: Position3D,
    },
    /// A device traveling indefinitely at constant velocity.
    Linear {
        /// Known position at `epoch_ns`.
        position_at_epoch: Position3D,
        /// Constant Cartesian velocity.
        velocity_mps: Velocity3D,
        /// Virtual time corresponding to `position_at_epoch`.
        #[serde(default)]
        epoch_ns: u64,
    },
    /// A trajectory linearly interpolated between timed waypoints.
    Waypoints {
        /// Strictly time-ordered trajectory points.
        waypoints: Vec<Waypoint>,
    },
}

impl Default for MobilityModel {
    fn default() -> Self {
        Self::Static {
            position: Position3D::ORIGIN,
        }
    }
}

impl MobilityModel {
    /// Calculates the device position at a virtual time.
    ///
    /// Waypoint trajectories hold their first or last position outside the
    /// configured time range and interpolate linearly within it.
    #[must_use]
    pub fn position_at(&self, time: SimTime) -> Position3D {
        match self {
            Self::Static { position } => *position,
            Self::Linear {
                position_at_epoch,
                velocity_mps,
                epoch_ns,
            } => {
                let elapsed_seconds = (time.as_nanos() as f64 - *epoch_ns as f64) / 1_000_000_000.0;
                Position3D {
                    x_m: position_at_epoch.x_m + velocity_mps.x_mps * elapsed_seconds,
                    y_m: position_at_epoch.y_m + velocity_mps.y_mps * elapsed_seconds,
                    z_m: position_at_epoch.z_m + velocity_mps.z_mps * elapsed_seconds,
                }
            }
            Self::Waypoints { waypoints } => interpolate_waypoints(waypoints, time.as_nanos()),
        }
    }
}

fn interpolate_waypoints(waypoints: &[Waypoint], time_ns: u64) -> Position3D {
    let Some(first) = waypoints.first() else {
        return Position3D::ORIGIN;
    };
    if time_ns <= first.at_ns {
        return first.position;
    }
    let Some(last) = waypoints.last() else {
        return first.position;
    };
    if time_ns >= last.at_ns {
        return last.position;
    }

    let upper_index = waypoints.partition_point(|waypoint| waypoint.at_ns <= time_ns);
    let lower = waypoints[upper_index - 1];
    let upper = waypoints[upper_index];
    let fraction = (time_ns - lower.at_ns) as f64 / (upper.at_ns - lower.at_ns) as f64;
    Position3D {
        x_m: lower.position.x_m + (upper.position.x_m - lower.position.x_m) * fraction,
        y_m: lower.position.y_m + (upper.position.y_m - lower.position.y_m) * fraction,
        z_m: lower.position.z_m + (upper.position.z_m - lower.position.z_m) * fraction,
    }
}
