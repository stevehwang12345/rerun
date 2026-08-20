# RMS Product App

`rms_product_app`은 Rerun Viewer를 iframe이나 npm wrapper로 감싸지 않고 `re_viewer::App`을 소스 수준에서 조립하는 제품 runtime입니다.

프로젝트, 디바이스, 데이터, 토픽, Viewer preset, LIVE/Replay 상태, 제어 UX는 Rerun viewport와 같은 egui application 안에서 렌더링됩니다.

Rerun EntityDB, Query, Transform, Blueprint, Time Control, `re_renderer`, wgpu는 upstream 구현을 그대로 사용합니다.

Web build는 `RmsWebHandle`의 `start`, `set_event_callback`, `apply_control_response`, `stop` 계약을 노출합니다.

물리 명령은 Viewer canvas에서 직접 전송하지 않고 product event로 RMS HTTP adapter에 전달됩니다.

Adapter는 lease ID, lease epoch, device state version, idempotency key, 명령 TTL을 포함한 기존 RMS API 계약을 적용합니다.

Rerun의 일반 top, blueprint, selection, time panel은 기본 숨김이며 사용자는 현재 작업에 필요한 정보만 봅니다.

Native 실행 파일은 `cargo run -p rms_product_app --features native`로 명시적으로 활성화합니다.
