use opengobo_protocol::{AttributeKind, DmxAddress, FixtureAttributeMap};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

pub const DMX_UNIVERSE_SIZE: usize = 512;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FixtureError {
    #[error("mode '{0}' not found in fixture '{1}'")]
    ModeNotFound(String, String),
    #[error("channel '{0}' in mode '{1}' is not defined")]
    ChannelNotFound(String, String),
    #[error("fixture footprint ({footprint} channels at {start}) exceeds 512 channels")]
    AddressOutOfBounds { start: u16, footprint: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OflChannel {
    pub attribute: AttributeKind,
    /// Optional fine channel name for 16-bit resolution (coarse + fine).
    #[serde(default)]
    pub fine_channel: Option<String>,
    #[serde(default)]
    pub default_value: u8,
    #[serde(default)]
    pub invert: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OflMode {
    pub name: String,
    #[serde(default)]
    pub short_name: Option<String>,
    pub channels: Vec<String>,
}

/// Open Fixture Library (OFL) compatible fixture definition embedded in `.gobo` files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OflFixtureDefinition {
    pub key: String,
    pub name: String,
    pub manufacturer: String,
    pub available_channels: BTreeMap<String, OflChannel>,
    pub modes: Vec<OflMode>,
}

impl OflFixtureDefinition {
    #[must_use]
    pub fn generic_dimmer_rgb_par() -> Self {
        let specs = [
            ("Dimmer", AttributeKind::Dimmer),
            ("Red", AttributeKind::ColorRed),
            ("Green", AttributeKind::ColorGreen),
            ("Blue", AttributeKind::ColorBlue),
        ];

        let available_channels = specs
            .iter()
            .map(|(name, attr)| {
                (
                    (*name).to_string(),
                    OflChannel {
                        attribute: attr.clone(),
                        fine_channel: None,
                        default_value: 0,
                        invert: false,
                    },
                )
            })
            .collect();

        Self {
            key: "generic/dimmer-rgb-par".to_string(),
            name: "Generic Dimmer RGB PAR".to_string(),
            manufacturer: "Generic".to_string(),
            available_channels,
            modes: vec![OflMode {
                name: "4ch".to_string(),
                short_name: Some("4ch".to_string()),
                channels: specs.iter().map(|(n, _)| (*n).to_string()).collect(),
            }],
        }
    }

    pub fn compile_mode(&self, mode_name: &str) -> Result<FixtureProfile, FixtureError> {
        let mode = self
            .modes
            .iter()
            .find(|m| m.name == mode_name || m.short_name.as_deref() == Some(mode_name))
            .ok_or_else(|| FixtureError::ModeNotFound(mode_name.to_string(), self.key.clone()))?;

        let footprint = mode.channels.len();
        let mut defaults = vec![0u8; footprint];
        let mut mappings = Vec::new();

        for (offset, ch_name) in mode.channels.iter().enumerate() {
            if let Some(ch_def) = self.available_channels.get(ch_name) {
                defaults[offset] = ch_def.default_value;
                let fine_offset = ch_def
                    .fine_channel
                    .as_ref()
                    .and_then(|fine| mode.channels.iter().position(|c| c == fine));

                mappings.push(CompiledChannelMapping {
                    attribute: ch_def.attribute.clone(),
                    coarse_offset: offset,
                    fine_offset,
                    invert: ch_def.invert,
                });
            } else if !self
                .available_channels
                .values()
                .any(|c| c.fine_channel.as_deref() == Some(ch_name.as_str()))
            {
                return Err(FixtureError::ChannelNotFound(
                    ch_name.clone(),
                    mode.name.clone(),
                ));
            }
        }

        Ok(FixtureProfile {
            fixture_key: self.key.clone(),
            mode_name: mode.name.clone(),
            footprint,
            defaults,
            mappings,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledChannelMapping {
    pub attribute: AttributeKind,
    pub coarse_offset: usize,
    pub fine_offset: Option<usize>,
    pub invert: bool,
}

/// Pre-compiled runtime profile for fast per-tick DMX rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureProfile {
    pub fixture_key: String,
    pub mode_name: String,
    pub footprint: usize,
    pub defaults: Vec<u8>,
    pub mappings: Vec<CompiledChannelMapping>,
}

impl FixtureProfile {
    pub fn render_into_universe(
        &self,
        start_address: DmxAddress,
        attributes: &FixtureAttributeMap,
        universe: &mut [u8; DMX_UNIVERSE_SIZE],
    ) -> Result<(), FixtureError> {
        let base = start_address.zero_based_index();
        let end = base.saturating_add(self.footprint);
        if end > DMX_UNIVERSE_SIZE {
            return Err(FixtureError::AddressOutOfBounds {
                start: start_address.get(),
                footprint: self.footprint,
            });
        }

        universe[base..end].copy_from_slice(&self.defaults);

        for m in &self.mappings {
            let Some(&val) = attributes.get(&m.attribute) else {
                continue;
            };
            if let Some(fine_offset) = m.fine_offset {
                let [coarse, fine] = val.to_u16_be(m.invert);
                universe[base + m.coarse_offset] = coarse;
                universe[base + fine_offset] = fine;
            } else {
                universe[base + m.coarse_offset] = val.to_u8(m.invert);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opengobo_protocol::NormalizedValue;

    #[test]
    fn patches_8bit_and_16bit_attributes_correctly() {
        let available_channels = BTreeMap::from([
            (
                "Pan".to_string(),
                OflChannel {
                    attribute: AttributeKind::Pan,
                    fine_channel: Some("Pan fine".to_string()),
                    default_value: 128,
                    invert: false,
                },
            ),
            (
                "Dimmer".to_string(),
                OflChannel {
                    attribute: AttributeKind::Dimmer,
                    fine_channel: None,
                    default_value: 0,
                    invert: true,
                },
            ),
        ]);

        let def = OflFixtureDefinition {
            key: "test/moving-head".to_string(),
            name: "Test MH".to_string(),
            manufacturer: "OpenGobo".to_string(),
            available_channels,
            modes: vec![OflMode {
                name: "3ch".to_string(),
                short_name: None,
                channels: vec![
                    "Pan".to_string(),
                    "Pan fine".to_string(),
                    "Dimmer".to_string(),
                ],
            }],
        };

        let profile = def
            .compile_mode("3ch")
            .unwrap_or_else(|e| panic!("failed to compile mode: {e}"));
        assert_eq!(profile.footprint, 3);

        let attrs = FixtureAttributeMap::from([
            (AttributeKind::Pan, NormalizedValue::FULL),
            (AttributeKind::Dimmer, NormalizedValue::FULL),
        ]);

        let mut dmx = [0u8; DMX_UNIVERSE_SIZE];
        let addr = DmxAddress::new(10).unwrap_or_else(|| panic!("valid address"));
        assert!(profile.render_into_universe(addr, &attrs, &mut dmx).is_ok());

        assert_eq!(&dmx[9..12], &[255, 255, 0]);
    }
}
