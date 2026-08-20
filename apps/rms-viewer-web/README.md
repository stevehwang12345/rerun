# RMS Viewer Web

RMS Viewer Web은 `rms_product_app` Rust crate가 소유하는 통합 Viewer의 최소 React host입니다.
React는 하나의 canvas를 제공하고 fork에서 생성한 Wasm module을 시작하며, Viewer와 운영 UI의 상태는 Rust product runtime이 소유합니다.

## 실행

```powershell
cd C:\Rerun\apps\rms-viewer-web
npm install
npm run dev
```

기본 주소는 `http://127.0.0.1:4173`입니다.
`npm run dev`는 debug Wasm artifact를 `public/runtime`에 생성한 뒤 Vite를 시작합니다.

Wasm artifact를 이미 만든 경우에는 host만 시작할 수 있습니다.

```powershell
npm run dev:host
```

생성된 `public/runtime` 파일은 Git에 커밋하지 않습니다.

## Backend 연동

Rust product runtime은 제어 의도, Viewer 시간, 선택 상태, 제어 가능 여부를 소유합니다.

React host는 `RmsApi`에서 프로젝트, 장비, 데이터 소스와 Topic을 읽어 인증된 Viewer context로 전달합니다.

제어 요청은 request ID와 device ID를 왕복시키며 Lease 대상, 소유자, epoch와 만료 시각이 현재 화면과 일치할 때만 Rust runtime이 수락합니다.

`VITE_RMS_API_BASE`가 없으면 안전 계약을 동일하게 구현한 `MockRmsApi`가 사용됩니다.

운영 환경에서는 `/api`와 `/rerun`을 동일 origin으로 제공하고 `RMS_BACKEND_TARGET`으로 로컬 proxy 대상을 지정합니다.

Replay나 일시정지 상태에서는 Viewer가 명령을 만들지 않으며 LIVE를 벗어날 때 보유 제어권을 반납합니다.

## 검증

```powershell
npm test
npm run build
```

Rust artifact 없이 React/TypeScript host만 검증하려면 `npm run build:host`를 사용합니다.
기존 API와 domain 테스트는 host 전환 중에도 안전 계약 회귀 검증을 위해 유지합니다.
Release Wasm 최적화에는 `wasm-opt`를 제공하는 Binaryen 또는 저장소의 Pixi 환경이 필요합니다.
