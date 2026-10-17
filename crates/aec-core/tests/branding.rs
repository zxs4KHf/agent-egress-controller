#[test]
fn application_icon_is_neutral_purple_not_a_green_readiness_indicator() {
    let ico = aec_core::brand::ico();
    let offset = 22 + 40 + ((63 - 32) * 64 + 8) * 4;
    assert_eq!(
        &ico[offset..offset + 4],
        &[154, 91, 99, 255],
        "Static app branding must not look like the tray's green safe/ready state"
    );
}

#[test]
fn simplified_chinese_is_an_explicit_browser_language_choice() {
    assert!(
        aec_core::setup::LANGUAGES
            .iter()
            .any(|(tag, _)| *tag == "zh-CN"),
        "Returning to Chinese must not require choosing Traditional Chinese"
    );
}
