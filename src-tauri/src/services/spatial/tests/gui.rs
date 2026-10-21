use super::super::{
    MapGuiBackend, MapSpatialCapability, RegistryMapSpatialCapability, UnavailableMapGui,
    WeakLocalIpcGui,
};
use super::support::saved;
use crate::services::{backend::DriverContext, contracts::ErrorCode, local_ipc::GuiApplyRequest};
use serde_json::json;
#[tokio::test]
async fn 내장_드라이버의_일반_기능을_공간_capability로_추측하지_않는다() {
    // given
    crate::drivers::registry::register_driver(crate::drivers::sqlite::SqliteDriver::new()).await;
    let mut connection = saved("conn-a");
    connection.driver = "sqlite".into();
    let context = DriverContext {
        connection_id: "conn-a".into(),
        request_id: "spatial-capability".into(),
        deadline_ms: 1000,
        signal: Default::default(),
        read_only: true,
        session_handle: None,
        transient_auth: None,
    };
    // when
    let actual = RegistryMapSpatialCapability
        .supports(&connection, &context)
        .await;
    // then
    assert!(!actual.unwrap());
}
async fn unavailable_gui() -> (usize, ErrorCode, ErrorCode) {
    let default = UnavailableMapGui;
    let count = default.discover().await.unwrap().len();
    let weak = WeakLocalIpcGui::new(std::sync::Weak::new());
    let discovery = weak.discover().await.err().unwrap().code;
    let apply = weak
        .apply(
            "gui-a",
            GuiApplyRequest {
                map_id: "map-a".into(),
                state_version: 0,
                generation: 0,
                state: json!({"action":"open","map":{}}),
            },
        )
        .await
        .err()
        .unwrap()
        .code;
    (count, discovery, apply)
}
#[tokio::test]
async fn ipc가_종료된_약한_gui_어댑터는_기본_대상을_선택하거나_실행하지_않는다() {
    // given
    // when
    let actual = unavailable_gui().await;
    // then
    assert_eq!(
        actual,
        (0, ErrorCode::GuiUnavailable, ErrorCode::GuiUnavailable)
    );
}
