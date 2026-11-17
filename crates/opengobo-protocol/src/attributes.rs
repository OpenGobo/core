use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Logical fixture attributes decoupled from raw DMX channel numbers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributeKind {
    Dimmer,
    ColorRed,
    ColorGreen,
    ColorBlue,
    ColorWhite,
    ColorAmber,
    ColorUv,
    Pan,
    Tilt,
    Strobe,
    Zoom,
    Focus,
    Gobo,
    Custom(String),
}

impl AttributeKind {
    /// Dimmer uses Highest-Takes-Precedence (HTP) across scenes; all other attributes use LTP.
    #[must_use]
    pub const fn is_htp(&self) -> bool {
        matches!(self, Self::Dimmer)
    }
}

/// Normalized attribute value clamped to `0.0..=1.0`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct NormalizedValue(f32);

impl NormalizedValue {
    pub const ZERO: Self = Self(0.0);
    pub const FULL: Self = Self(1.0);

    #[must_use]
    pub fn new(value: f32) -> Self {
        if value.is_finite() {
            Self(value.clamp(0.0, 1.0))
        } else {
            Self::ZERO
        }
    }

    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }

    fn effective(self, invert: bool) -> f32 {
        if invert { 1.0 - self.0 } else { self.0 }
    }

    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn to_u8(self, invert: bool) -> u8 {
        (self.effective(invert) * 255.0).round() as u8
    }

    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn to_u16_be(self, invert: bool) -> [u8; 2] {
        ((self.effective(invert) * 65535.0).round() as u16).to_be_bytes()
    }
}

pub type FixtureAttributeMap = BTreeMap<AttributeKind, NormalizedValue>;
