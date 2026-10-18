//! The fixtures are synthetic.
mod common;

use common::*;
use plist::Value;

const FORBIDDEN: &[&str] = &[
    "Target Identifier",
    "Serial Number",
    "Phone Number",
    "IMEI",
    "IMEI 2",
    "ICCID",
    "MEID",
    "GUID",
];

#[test]
fn are_synthetic() {
    for name in ALL_FIXTURES {
        if *name == "bomb-info" {
            // A reference bomb: reading it with plist::Value would expand it.
            continue;
        }
        let info = Value::from_file(fixtures_dir().join(name).join(UDID).join("Info.plist"))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let dict = info.as_dictionary().expect("dictionary");
        let device = dict
            .get("Device Name")
            .and_then(Value::as_string)
            .expect("Device Name");
        assert!(device.starts_with("Synthetic"), "{name}");
        assert_eq!(
            dict.get("Unique Identifier").and_then(Value::as_string),
            Some(UDID),
            "{name}"
        );
        for key in FORBIDDEN {
            assert!(dict.get(key).is_none(), "{name}: {key}");
        }
    }
}
