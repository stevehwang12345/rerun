# RMS Web Viewer MVP 구현 계획

> 기준: Rerun 0.36.1 Web Viewer와 하나의 RMS 운영 화면

## 확인 결과

현재 저장소에는 Rerun 데이터 서버와 Web Viewer는 있지만 프로젝트, 디바이스, Topic mapping 및 물리 장비 제어를 담당하는 RMS 백엔드는 없다.
Rerun `re_grpc_server`와 Redap은 RRD 및 Chunk 데이터 경로로 유지하고 제품 메타데이터와 제어는 RMS API가 담당해야 한다.
iframe과 `@rerun-io/web-viewer` wrapper는 사용하지 않는다.
`rms_product_app`이 `re_viewer::App`을 직접 조립하고 Native와 Web에서 동일한 egui 제품 UI를 소유한다.
React host는 canvas, Wasm 부팅, 인증된 RMS API transport만 담당한다.

## 사용자 시나리오

1. 사용자가 프로젝트를 선택한다.
2. 프로젝트에 속한 디바이스와 연결 상태만 확인한다.
3. 디바이스의 LIVE 또는 Recording을 선택한다.
4. 운영, 카메라 또는 진단 Preset이 필요한 Topic만 표시한다.
5. LIVE, 데이터 정상, 장비 정상 및 본인 Lease 조건이 모두 충족될 때만 제어한다.
6. Replay나 일시정지 중에는 제어를 차단하고 `LIVE로 이동`만 제공한다.

## 화면 정보 예산

- 상단은 장비, LIVE/REPLAY, 종합 상태, 배터리, 작업과 제어권만 표시한다.
- 오른쪽 Topic Panel은 Preset이 선택한 정보만 표시한다.
- Topic schema, Entity path와 protocol code는 진단 Preset에서만 표시한다.
- Rerun 기본 Top, Blueprint 및 Selection Panel은 숨긴다.
- Replay에서는 내장 Time Panel을 29px 축소 상태로 표시하고 전체 Timeline은 사용자가 요청할 때만 펼친다.
- 로딩과 오류는 기술 상세 대신 한두 줄의 운영자 문구로 덮어쓴다.

## 데이터 경로

```text
RMS Product Runtime
├── React transport     /api/v1 Project, Device, Source, Topic, Lease, Command
├── Rust/egui product   Project, Device, Topic, Viewer, Time, Control eligibility
└── Rerun runtime       /rerun RRD, Redap 또는 gRPC data path
```

RMS API는 Chunk payload를 JSON으로 변환하지 않는다.
DataSource의 `rrdUrl`이 Rerun URI를 가리키고 Viewer가 해당 데이터 경로를 직접 사용한다.

## 최소 API 계약

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/v1/projects` | 프로젝트 목록 |
| GET | `/api/v1/projects/{id}/devices` | 프로젝트 장비 목록 |
| GET | `/api/v1/projects/{id}/data-sources?device_id=` | LIVE와 Recording 목록 |
| GET | `/api/v1/devices/{id}/topics?data_source_id=` | Topic과 Viewer mapping |
| GET | `/api/v1/devices/{id}/control-state` | 현재 Lease와 제어 상태 |
| POST | `/api/v1/devices/{id}/control-leases` | 제어권 요청 |
| DELETE | `/api/v1/control-leases/{lease_id}` | 제어권 반납 |
| POST | `/api/v1/devices/{id}/commands` | 검증된 명령 접수 |
| GET | `/api/v1/events?project_id=&device_id=&data_source_id=` | 선택한 데이터의 상태, Topic, Lease와 명령 결과 SSE |

모든 변경 요청에는 `Idempotency-Key`와 요청 추적 ID를 사용한다.
제어권 요청에는 `expectedDeviceVersion`을 포함하고, 모든 명령에는 `expectedDeviceVersion`, `leaseId`, `leaseEpoch`, `issuedAt`, `expiresAt`과 `sessionMode`를 포함한다.
서버는 사용자가 전송한 `sessionMode`를 신뢰하지 않고 최신 Live 상태와 Edge 상태를 다시 검증한다.
SSE의 Topic 이벤트는 `data_source_id` 범위로 제한하여 Replay 화면에 현재 LIVE 값이 섞이지 않게 한다.

## 구현 단계

- Phase 1은 현재 구현된 `rms_product_app`, 최소 Web host, REST catalog adapter, Mock API transport와 제어 차단 정책이다.
- Phase 2는 실제 RMS 서버의 SSE 상태 동기화, 전체 프로젝트 선택과 Preset persistence이다.
- Phase 3은 Rerun Blueprint 생성기와 Topic-to-Entity mapping이다.
- Phase 4는 Simulator 및 Edge Safety Agent를 통한 제어다.
- Phase 5에서 HIL 안전 검증 후 제한된 실장비를 연결한다.

## 현재 제한

로컬 LIVE는 footer가 검증된 50초 RRD fixture를 사용하므로 실제로 증가하는 Stream은 아니다.
Replay의 timeline, cursor, play state, speed와 loop 초기화는 `TimeControlCommand`로 연결되어 있으며 축소 재생 바와 전체 Timeline을 제공한다.
실제 Redap 연결 건강과 Blueprint preset 생성기는 후속 product seam이 필요하다.
현재 Rust 장비 선택기는 세 개의 검증용 ID로 제한되며 전체 프로젝트와 장비 목록의 동적 렌더링은 Phase 2 범위다.
실장비 명령은 구현하지 않았으며 Mock 명령도 Replay, Lease, 장비 상태와 state version 검증을 통과해야 한다.
