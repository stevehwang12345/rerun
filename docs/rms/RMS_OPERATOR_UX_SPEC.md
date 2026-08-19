# RMS 실사용자 화면 명세

> 목표: 설명을 읽지 않아도 현재 장비 상태와 다음 행동을 판단할 수 있는 통합 관제·제어 화면

## 1. 화면 원칙

- 하나의 앱에서 Fleet, Mission, Live, Replay, 제어, Incident를 처리한다.
- 기본 화면에는 현재 작업에 필요한 정보만 표시한다.
- 기술 용어와 내부 구조는 기본 화면에서 숨긴다.
- 정상 상태는 조용하게, 위험 상태는 명확하고 행동 가능하게 표시한다.
- 색상만으로 상태를 표현하지 않는다.
- 위험도에 따라 확인 강도를 다르게 적용한다.
- Replay와 Live를 어떤 상황에서도 혼동할 수 없게 한다.

### 1.1 화면 노출량 예산

- 정상 운용 화면에는 설명 문단을 상시 표시하지 않는다.
- 한 상태에서 강조하는 주 행동은 하나만 둔다. 보조 행동은 메뉴나 상세 drawer로 내린다.
- 상태·경고 문구는 원칙적으로 제목 한 줄과 영향/행동 한 줄 이내로 제한한다.
- 같은 원인의 센서·네트워크·프로토콜 오류는 사용자에게 하나의 운용 상태로 합친다.
- 도움말은 처음부터 펼치지 않고 label의 tooltip, 도움말, `기술 세부정보` 순서로 요청할 때만 공개한다.
- 빈 화면 안내와 onboarding도 현재 해야 할 행동 한 문장만 보여 준다.
- Engineer 진단 화면을 열기 전에는 raw ID, protocol code, metric table, stack trace를 렌더링하지 않는다.

## 2. 기본 정보 구조

### 2.1 최상위 메뉴

```text
Fleet | Operations | Incidents | Recordings | Admin
```

Operator에게는 기본적으로 `Operations`만 열린다. 권한이 없는 메뉴는 disabled로 나열하지 않고 숨긴다.

### 2.2 Operations 화면

```text
┌──────────────────────────────────────────────────────────────────────┐
│ Robot-07   ● LIVE   Autonomous   정상   제어권: 나   Mission 42     │
├───────────────────────────────────────────────┬──────────────────────┤
│                                               │ 현재 작업            │
│            3D / Map / Camera                  │ Bay 3 이동           │
│                                               │ 진행 62%             │
│                                               │                      │
│                                               │ [일시정지] [중지]    │
├───────────────────────────────────────────────┴──────────────────────┤
│ 14:32:10 ───────────────● LIVE       경고 1: 전방 경로 재계산 중    │
└──────────────────────────────────────────────────────────────────────┘
```

항상 표시:

- 장비명
- `LIVE` 또는 `REPLAY`
- 장비 운용 mode
- 종합 health
- 제어권 보유자
- 현재 Mission/Task
- 가장 중요한 경고 1개

이 항목은 짧은 label과 값으로만 표시한다. 각 항목의 정의나 시스템 동작 설명을 옆에 상시 붙이지 않는다.

기본 숨김:

- Entity tree 전체
- Blueprint 편집기
- Chunk/Store browser
- Query 정보
- QoS/Network 상세 metric
- Raw protocol ACK
- Stack trace
- VRAM/RAM 내부 breakdown

## 3. 사용자 역할별 공개 수준

| 역할 | 기본 화면 | 추가 화면 |
|---|---|---|
| Operator | 상태, Mission, 필요한 제어, 경고 | 간단한 command 이력 |
| Supervisor | Operator 화면 + 승인/인계 | Incident와 안전 개입 요약 |
| Engineer | 진단 Preset | Sensor/QoS/TF/Chunk/GPU 상세 |
| Administrator | 관리 화면 | 계정, 정책, 인증서, 보존 |

권한과 정보 공개 수준을 함께 제어한다. `고급 보기` 버튼 하나만으로 Engineer 정보가 노출되지 않는다.

## 4. 상태 표현

### 4.1 Live/Replay

| 상태 | 화면 처리 | 제어 |
|---|---|---|
| Live head | `● LIVE` | 조건 충족 시 가능 |
| Paused live | `일시정지 · LIVE에서 8초 전` | 불가 |
| Replay | 상단과 Timeline에 `REPLAY` 고정 | 불가 |
| Return-to-live 중 | `LIVE로 이동 중` | 불가 |
| Live 복귀 완료 | `● LIVE` + 제어 재개 버튼 | 명시적 재개 후 가능 |

긴 안내문이나 반복 popup을 사용하지 않는다. 지속적인 상태 label과 제어 영역 비활성화로 전달한다.

### 4.2 Health

```text
정상
주의 — 운용 가능
제한 — 일부 기능 중지
위험 — 안전 동작 실행 중
연결 끊김
```

기술 오류 여러 개를 종합 health 하나로 요약한다. 세부 원인은 펼쳐보기에서 severity순으로 제공한다.

### 4.3 Command 결과

내부 상태를 다음 사용자 언어로 변환한다.

| 내부 상태 | 화면 문구 |
|---|---|
| Requested/AwaitingApproval | 승인 대기 |
| Authorized/Dispatched | 장비로 전달 중 |
| Accepted | 장비가 요청을 받음 |
| Executing | 실행 중 |
| Succeeded | 완료 |
| Rejected | 실행할 수 없음 |
| Failed | 실행 실패 |
| OutcomeUnknown | 결과 확인 필요 |

`Accepted`를 `완료`로 표시하지 않는다.

## 5. 제어 UX

### 5.1 제어권

제어권이 없을 때는 모든 버튼을 단순 disabled 처리하지 않고 핵심 위치에 다음 하나만 표시한다.

```text
현재 보기 전용                    [제어권 요청]
```

제어권 획득 후:

```text
제어권: 나 · 01:42 남음            [반납]
```

기술적인 Lease ID, epoch, fencing token은 숨긴다.

### 5.2 위험도별 확인

| 위험도 | 동작 | UX |
|---|---|---|
| 낮음 | PTZ, 조명 | 즉시 실행, Undo 가능 시 제공 |
| 중간 | Mission pause/resume, 목적지 | preview 후 한 번 확인 |
| 높음 | Arm, takeoff, remote mode | 상태 요약 + hold-to-confirm/승인 |
| 비상 | Controlled stop/RTL | 항상 접근 가능, 동작을 명확히 표기 |
| 파괴적 | Kill/flight termination | 일반 화면에서 숨김, 별도 권한/절차 |

확인창에는 위험 설명 전체가 아니라 다음만 표시한다.

```text
Drone-03 이륙
목표 고도 20 m · Battery 86% · GPS 정상

[취소]                         [2초간 눌러 이륙]
```

### 5.3 3D/Map 명령

1. 목표 선택
2. 예상 경로를 화면에 표시
3. 위험 구간이 있으면 해당 구간만 강조
4. 거리/예상 시간/중요 제한을 한 줄로 표시
5. 확인 후 실행

검증 실패 시:

```text
이 위치로 이동할 수 없습니다.
안전 구역 밖에 있습니다.                         [다른 위치 선택]
```

Protocol code와 좌표 변환 상세는 `기술 세부정보`에 둔다.

## 6. 경고와 Incident

경고 카드 형식:

```text
전방 카메라 영상이 2초 지연되고 있습니다.
원격 조작을 일시 중지했습니다.                    [상세]
```

규칙:

- 하나의 원인은 하나의 경고로 묶는다.
- 같은 경고의 반복 toast를 금지한다.
- 사용자가 할 수 있는 행동이 없으면 불필요한 버튼을 만들지 않는다.
- 자동 안전 동작이 실행됐으면 결과를 먼저 표시한다.
- Incident 생성 시 관련 Timeline 범위, 장비, command를 자동 연결한다.

## 7. Preset

| Preset | 기본 View |
|---|---|
| Robot Operations | 3D/Map, Camera, Task, 상태 |
| Drone Flight | Map, FPV, altitude/battery, flight command |
| Autonomous Vehicle | Map/3D, Camera, route, operation mode |
| Inspection | 다중 Camera, checklist, capture |
| Incident Replay | Timeline, event list, Camera/3D 동기화 |
| Engineering Diagnostics | Rerun 상세 Panel과 metric |

운영 Preset은 사용자가 실수로 핵심 View를 제거하지 못하도록 layout lock을 지원한다. 개인 zoom/camera 배치는 저장할 수 있지만 조직 표준 상태/제어 영역은 고정한다.

## 8. 문구 가이드

사용:

- `데이터가 3초 지연됨`
- `제어권이 만료됨`
- `장비가 요청을 거부함`
- `LIVE로 돌아가기`
- `결과 확인 필요`

기본 화면에서 사용하지 않음:

- `latest-at query failed`
- `chunk evicted`
- `fencing token mismatch`
- `Redap manifest unavailable`
- `MAV_RESULT_TEMPORARILY_REJECTED`

기술 세부정보에는 원문 code를 보존해 Engineer가 문제를 진단할 수 있게 한다.

## 9. 화면 Acceptance 기준

- 처음 사용하는 Operator가 설명서 없이 장비, Live/Replay, health, 제어권을 5초 안에 식별한다.
- Replay 상태에서 제어 가능하다고 오인하는 사용자가 없어야 한다.
- 정상 운용 화면에 동시에 노출되는 경고는 중요도 기준 최대 3개다.
- 저위험 반복 동작에 불필요한 confirmation dialog가 없다.
- 고위험 동작은 대상과 결과를 확인하지 않고 실행할 수 없다.
- 상세 진단 정보가 Operator의 주 작업을 가리지 않는다.
- 키보드, screen reader, 색각 이상 환경에서도 핵심 상태를 구분할 수 있다.
