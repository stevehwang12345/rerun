# RMS Viewer Web

RMS Viewer Web은 Rerun 0.36.1 Web Viewer를 프로젝트, 디바이스, 데이터, Topic별 운영 화면과 결합하는 제품 Shell MVP입니다.
일반 운영 화면에는 장비, LIVE/REPLAY, 상태, 제어권, 현재 작업과 필요한 행동만 표시합니다.
Rerun의 Blueprint, Selection, Top Panel은 기본적으로 숨기고 Replay일 때만 Timeline을 펼칩니다.

## 실행

```powershell
cd C:\Rerun\apps\rms-viewer-web
npm install
npm run dev
```

기본 주소는 `http://127.0.0.1:4173`입니다.
환경변수를 설정하지 않으면 Mock 프로젝트와 장비를 사용하고 공개 Rerun 0.36.1 예제 RRD를 불러옵니다.

## 실제 백엔드 연결

`.env.local`에 다음 값을 설정합니다.

```dotenv
VITE_RMS_API_BASE=/api
VITE_RMS_USE_MOCK=false
RMS_BACKEND_TARGET=http://127.0.0.1:8080
```

Vite는 `/api`와 `/rerun`을 `RMS_BACKEND_TARGET`으로 프록시합니다.
운영 배포에서도 RMS API와 Rerun 데이터 경로를 동일 origin의 BFF 또는 reverse proxy 뒤에 두는 구성을 권장합니다.

## 검증

```powershell
npm test
npm run build
```

Mock은 실장비로 자동 fallback하지 않습니다.
실제 명령은 별도 RMS Control Core와 Edge Safety Agent가 Lease, 상태 버전, TTL, idempotency와 안전 정책을 검증한 뒤 실행해야 합니다.
