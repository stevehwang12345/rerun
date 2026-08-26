# RMS Viewer Web

RMS Viewer Web은 연동, 프로젝트, 실시간, 기록 서비스를 하나의 제품 Shell로 연결합니다.
React는 경로, 인증된 서비스 컨텍스트, HTTP와 SSE 전송을 담당합니다.
`rms_product_app`은 같은 페이지의 단일 canvas에서 Rerun Viewer와 장비별 Topic, Live 제어 UI를 소유합니다.

## 실행

```powershell
cd C:\Rerun\apps\rms-viewer-web
npm install
npm run dev
```

기본 주소는 `http://127.0.0.1:4173`입니다.
`npm run dev`는 debug Wasm artifact를 생성하고 `rms_server`와 Vite를 함께 시작합니다.

로컬 통합 시스템은 Backend와 Web 앱을 함께 실행합니다.

```powershell
npm run dev:system
```

`dev:system`은 `rms_server`를 확인하거나 시작하고 `/api`와 `/rerun`을 동일 origin으로 연결합니다.

명시적인 in-memory 데모만 실행하려면 다음 명령을 사용합니다.

```powershell
npm run dev:mock
```

Wasm artifact를 이미 만든 경우에는 host만 시작할 수 있습니다.

```powershell
npm run dev:host
```

생성된 `public/runtime` 파일은 Git에 커밋하지 않습니다.

## Backend 연동

Rust product runtime은 제어 의도, Viewer 시간, 선택 상태, 제어 가능 여부를 소유합니다.

React host는 Connect Registry와 Project Assignment를 읽고 LiveSession 또는 ReplaySession을 만든 뒤 인증된 Viewer context를 전달합니다.

제어 요청은 request ID와 device ID를 왕복시키며 Lease 대상, 소유자, epoch와 만료 시각이 현재 화면과 일치할 때만 Rust runtime이 수락합니다.

Mock은 `VITE_RMS_USE_MOCK=true`인 경우에만 사용하며 기본 API 주소는 `/api`입니다.

운영 환경에서는 `/api`와 `/rerun`을 동일 origin으로 제공하고 `RMS_BACKEND_TARGET`으로 로컬 proxy 대상을 지정합니다.

## 파일 가져오기

`연동` 화면의 `파일 가져오기…`에서 프로젝트와 장비를 고른 뒤 RRD, MCAP, ROS 2 Bag ZIP, CSV 또는 MP4/MOV/WebM 영상을 선택합니다.
업로드와 변환이 끝나면 해당 프로젝트의 `기록`에 Recording이 추가되고, 같은 화면에서 바로 `Replay 열기`로 이동할 수 있습니다.
화면에는 업로드·처리·준비 상태만 표시하며 자세한 변환 오류는 서버 로그에 남깁니다.
로컬 확인에는 `tests/assets/rms_import/telemetry.csv`를 사용할 수 있습니다.

Replay API와 Runtime에는 제어 capability가 없으며 LIVE를 벗어날 때 보유 제어권을 반납합니다.

Replay는 Rerun의 내장 Time Panel을 기본 축소 상태로 표시합니다.
축소 바에서 재생, 일시정지, cursor 이동과 속도를 조작하고 RMS 하단의 `타임라인` 버튼으로 전체 데이터 Timeline을 펼칠 수 있습니다.
초기 timeline, 손실 없는 cursor, play state, speed와 loop 정책은 ReplaySession에서 Rust Runtime으로 전달됩니다.

LIVE 제어 capability는 DataSource가 `recording` 상태이고 세션과 필수 Topic이 모두 `fresh`일 때만 주입됩니다.
프로젝트, 장비, Source 또는 Topic 상태가 바뀌면 Web host는 즉시 fail-closed context를 다시 적용합니다.

## 검증

```powershell
npm test
npm run build
cargo nextest run --all-features --no-fail-fast -p rms_server -p rms_product_app
target\debug\rerun.exe rrd verify tests\assets\rrd\rms\rms_replay_50s_v0_36_1.rrd
```

Rust artifact 없이 React/TypeScript host만 검증하려면 `npm run build:host`를 사용합니다.
API, domain, 서버와 제품 Runtime 테스트는 Live/Replay 분리와 안전 계약의 회귀를 검증합니다.
Release Wasm 최적화에는 `wasm-opt`를 제공하는 Binaryen 또는 저장소의 Pixi 환경이 필요합니다.
