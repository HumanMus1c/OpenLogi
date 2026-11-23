//! Custom device names, including two serial-less cameras of one model.

use super::*;

const CAMERA_A_ID: &str = "0x1123000046d0893";

const CAMERA_B_ID: &str = "0x14110000046d0893";

const SYNTHETIC_MOUSE_KEY: &str = "unit:4f4c4403";

fn synthetic_mouse_inventory() -> DeviceInventory {
    let mut inventory = direct_inventory([0x4f, 0x4c, 0x44, 0x03]);
    inventory.paired[0].codename = Some("Synthetic Direct Mouse".to_string());
    // No model info, so the resolver has no asset: the state before the
    // catalog answers.
    inventory.paired[0].model_info = None;
    inventory
}

fn product_identity(inventory: &DeviceInventory) -> DeviceIdentity {
    DeviceIdentity {
        display_name: "MX Vertical".to_string(),
        model_info: inventory.paired[0].model_info.clone(),
        codename: inventory.paired[0].codename.clone(),
        kind: DeviceKind::Mouse,
        capabilities: inventory.paired[0].capabilities.unwrap_or_default(),
        light_capabilities: None,
        driver_id: None,
        registry_model_id: None,
    }
}

fn serial_less_same_model_cameras() -> [Camera; 2] {
    let first = Camera {
        name: "Logitech StreamCam".to_string(),
        unique_id: CAMERA_A_ID.to_string(),
        serial_number: None,
        vendor_id: 0x046d,
        product_id: 0x0893,
        max_resolution: None,
        max_fps: None,
    };
    let second = Camera {
        unique_id: CAMERA_B_ID.to_string(),
        ..first.clone()
    };
    [first, second]
}

fn state_with_same_model_cameras(config: Config) -> AppState {
    let cameras = serial_less_same_model_cameras();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    AppState::new(Sources {
        cameras: &cameras,
        ..Sources::in_memory(config, &AssetResolver::new(), commands)
    })
}

fn camera_record<'a>(state: &'a AppState, capture_id: &str) -> &'a DeviceRecord {
    state
        .devices()
        .iter()
        .find(|record| record.capture_id.as_deref() == Some(capture_id))
        .expect("camera record")
}

#[test]
fn custom_device_name_updates_the_ui_and_can_restore_the_model_name() {
    let mut state = state_with_a_known_mouse();
    let model_name = state
        .current_record()
        .expect("known mouse")
        .model_name
        .clone();

    let _ = state.commit_device_custom_name(KNOWN_MOUSE_KEY, "  Office mouse  ");

    assert_eq!(
        state
            .current_record()
            .map(|record| record.display_name.as_str()),
        Some("Office mouse")
    );
    assert_eq!(
        state.config.device_custom_name(KNOWN_MOUSE_KEY),
        Some("Office mouse")
    );

    let _ = state.commit_device_custom_name(KNOWN_MOUSE_KEY, "   ");

    assert_eq!(
        state
            .current_record()
            .map(|record| record.display_name.as_str()),
        Some(model_name.as_str())
    );
    assert_eq!(state.config.device_custom_name(KNOWN_MOUSE_KEY), None);
}

#[test]
fn same_model_serial_less_cameras_keep_independent_names() {
    let mut state = state_with_same_model_cameras(Config::ephemeral());
    let second_key = camera_record(&state, CAMERA_B_ID).record_key();

    let _ = state.commit_device_custom_name(&second_key, "Desk camera");

    assert_eq!(
        camera_record(&state, CAMERA_A_ID).display_name,
        "Logitech StreamCam"
    );
    assert_eq!(
        camera_record(&state, CAMERA_B_ID).display_name,
        "Desk camera"
    );

    let restored = state_with_same_model_cameras(state.config.clone());
    assert_eq!(
        camera_record(&restored, CAMERA_A_ID).display_name,
        "Logitech StreamCam"
    );
    assert_eq!(
        camera_record(&restored, CAMERA_B_ID).display_name,
        "Desk camera"
    );
}

#[test]
fn a_slow_asset_catalog_keeps_the_persisted_product_name() {
    let inventory = synthetic_mouse_inventory();
    let mut config = Config::ephemeral();
    config.set_device_identity(SYNTHETIC_MOUSE_KEY, product_identity(&inventory));
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: std::slice::from_ref(&inventory),
        ..Sources::in_memory(config, &resolver, commands)
    });

    let record = state
        .devices()
        .iter()
        .find(|record| {
            record.config_key == SYNTHETIC_MOUSE_KEY
                || record.canonical_key.as_deref() == Some(SYNTHETIC_MOUSE_KEY)
        })
        .expect("synthetic mouse record");
    assert_eq!(record.display_name, "MX Vertical");
    assert_eq!(record.model_name, "MX Vertical");

    // The first refresh can run before the asset resolver has data; the
    // record then still carries the codename. Refreshing its identity must
    // not overwrite the product name the config already has.
    let mut records = state.devices().to_vec();
    let record = records
        .iter_mut()
        .find(|record| {
            record.config_key == SYNTHETIC_MOUSE_KEY
                || record.canonical_key.as_deref() == Some(SYNTHETIC_MOUSE_KEY)
        })
        .expect("synthetic mouse record");
    record.config_key = SYNTHETIC_MOUSE_KEY.to_string();
    record.canonical_key = Some(SYNTHETIC_MOUSE_KEY.to_string());
    record.persistent = true;
    record.model_name = "Synthetic Direct Mouse".to_string();
    record.asset = None;
    let mut config = state.config.clone();
    super::super::inventory::persist_identities(&mut config, &records);
    assert_eq!(
        config
            .device_identity(SYNTHETIC_MOUSE_KEY)
            .map(|identity| identity.display_name.as_str()),
        Some("MX Vertical")
    );
}
