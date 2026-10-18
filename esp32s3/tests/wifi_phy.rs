use esp_soc::wifi::{ApConfig, StaState, VirtualAp};
use esp32s3::periph::Peripherals;

// FE +0x174 bit 16 is inferred from the closed PHY driver's polling and the
// existing S3 model, not a documented IDF register or a silicon timing claim.
const IQ_RESULT: u32 = 0x6000_6174;
const DONE: u32 = 1 << 16;

#[test]
fn phy_completion_without_ap_does_not_create_a_link() {
    let mut p = Peripherals::new([0; 6]);
    for word in [0, 0x1234, 0xfffe_ffff, 0] {
        p.write32(IQ_RESULT, word);
        assert_eq!(p.read32(IQ_RESULT), word | DONE);
        assert!(p.wifi.link.ap().is_none());
        assert!(p.wifi.link.net().is_none());
        assert_eq!(p.wifi.link.rx_frames(), 0);
    }
}

#[test]
fn phy_completion_preserves_ap_state_and_survives_ap_removal() {
    let mut p = Peripherals::new([0; 6]);
    for state in [StaState::Idle, StaState::Associated] {
        let mut ap = VirtualAp::new(ApConfig::parse("ssid=fixture,chan=6").unwrap(), false);
        ap.state = state;
        p.wifi.link.attach(Some(ap), None);
        p.write32(IQ_RESULT, 0x1234);
        assert_eq!(p.read32(IQ_RESULT), 0x1234 | DONE);
        assert_eq!(p.wifi.link.ap().unwrap().state, state);
        assert!(p.wifi.link.net().is_none());
        p.wifi.link.attach(None, None);
        assert_eq!(p.read32(IQ_RESULT), 0x1234 | DONE);
    }
}
