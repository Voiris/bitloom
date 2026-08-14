use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GowinDevice {
    pub yosys_family: &'static str,
    pub nextpnr_family: &'static str,
}

impl GowinDevice {
    pub fn resolve(vendor: &str, device: &str) -> Result<Self, GowinConfigError> {
        if !vendor.eq_ignore_ascii_case("gowin") {
            return Err(GowinConfigError::UnsupportedVendor { vendor: vendor.to_owned() });
        }
        match device {
            "GW2AR-LV18QN88C8/I7" => Ok(Self { yosys_family: "gw2a", nextpnr_family: "GW2A-18C" }),
            _ => Err(GowinConfigError::UnsupportedDevice { device: device.to_owned() }),
        }
    }
}

#[derive(Debug, Error)]
pub enum GowinConfigError {
    #[error("unsupported FPGA vendor `{vendor}`; currently supported: gowin")]
    UnsupportedVendor { vendor: String },
    #[error("unsupported Gowin device `{device}`; currently supported: GW2AR-LV18QN88C8/I7")]
    UnsupportedDevice { device: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_tang_nano_20k_device() {
        assert_eq!(
            GowinDevice::resolve("gowin", "GW2AR-LV18QN88C8/I7").unwrap(),
            GowinDevice { yosys_family: "gw2a", nextpnr_family: "GW2A-18C" }
        );
    }
}
