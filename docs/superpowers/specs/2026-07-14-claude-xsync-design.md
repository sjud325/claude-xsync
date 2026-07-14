# claude-xsync 설계 문서

날짜: 2026-07-14
버전: v0.2 — 적대적 블라인드 리뷰(Critical 4 / Important 8 / Minor 6) 반영
상태: 사용자 승인 대기
언어: 이 문서는 리뷰용 한국어. 공개 README/문서는 영어로 작성한다.

## 1. 목표와 비목표

### 목표
- macOS(사용자 `woong`)와 **Windows 네이티브**(사용자 `Loki`) 두 기기 간 `~/.claude` 상태를 싱크한다.
- 두 기기가 Claude Code 관점에서 "완전히 같은 기기"처럼 동작한다: 세션(`--resume`), 설정, 에이전트/스킬, MCP 구성, 메모리 공유.
- **경로 문제가 절대 발생하지 않는다**: 사용자명 차이(woong/Loki), OS 차이(`/Users/...` vs `C:\Users\...`), 구분자 차이(`/` vs `\`), 대소문자 차이를 모두 처리한다.
- Windows에서 Git Bash와 PowerShell 어느 쪽에서 실행해도 동일하게 동작한다.
- 오픈소스(MIT)로 공개한다.

### 비목표 (의도적 제외)
- 동시 사용 / 실시간 싱크 / 파일 워처 (사용 패턴: 번갈아 사용, 수동 push/pull)
- 중앙 서버, 유저 간 공유, VSCode 확장 (Daniel 레포의 스코프 폭발 교훈)
- 다중 클라우드 백엔드 (전송은 git 하나)
- WSL 지원 고려 (요구사항에서 명시적 배제)

### 배경 (사전 조사 요약)
- **tawanorg/claude-sync** (Go, MIT): 가장 성숙한 기존 툴. 릴리스는 활발하나(v1.14.6, 2026-07-09), (1) `.releaserc.json` 에셋 목록 누락으로 **GitHub Release의 Windows 바이너리 부재가 약 2개월째 지속**(이슈 #31 open; npm 채널로는 win 패키지 배포 시도 중), (2) 경로 정규화(`internal/sync/paths.go:175-194`)가 홈 접두사만 바이트 치환하고 **구분자(`\`↔`/`)를 변환하지 않으며**, JSON 이스케이프된 `C:\\Users\\...` 형태를 매칭하지 못함 → Mac↔Windows 내용 경로 파손. 내용 변환 테스트는 유닉스 경로 위주.
- **claude-context-sync** (Python, 신생): 크로스 드라이브 템플릿 변수를 표방하나 `denormalize`(src/path_transformer.py:139)가 결과를 무조건 `\\`로 강제해 Mac 타깃이 깨짐. LICENSE 파일 부재.
- 결론: **크로스-OS 경로 정확성**이 두 툴 모두 실패한 지점이자 본 프로젝트의 존재 이유. 참조 클론: `~/workspace/claude-sync`, `~/workspace/claude-context-sync`.

### 실측 기준점 (설계 근거 데이터, 2026-07-14 woong 기기)
- `projects/` 277MB, 최대 단일 세션 156.6MB(.jsonl, gzip 후 ~30MB, 압축비 ~0.18), 최장 라인 0.39MB, 비JSON 라인 0.
- 세션 중 3개가 `C:\Users`/`C:/Users` 형태 문자열 포함(따옴표 인용 — 본 프로젝트 논의 세션 포함). 리터럴 `${HOME}` 포함 세션 현재 0개이나 본 툴 개발 세션에서 필연적으로 발생 예정.
- `~/.claude.json` 최상위 키 70+개(machineID, oauthAccount 등 머신 정체성 다수), `mcpServers`는 최상위 dict, `projects` dict 항목에 `allowedTools`/`enabledMcpServers`/`hasTrustDialogAccepted` 존재.
- `sessions/<pid>.json` = 실행 중 프로세스 레지스트리(pid, procStart, entrypoint) — 라이브 머신 상태.

## 2. 확정 결정

| 항목 | 결정 | 근거 |
|---|---|---|
| 이름 | `claude-xsync` | x = cross-platform |
| 언어 | Rust | 경로 shape을 enum + exhaustive match로 모델링 → 케이스 누락이 컴파일 에러. `Result`로 토큰 미해결 무시 불가능 |
| 전송 | private Git 레포 (shell-out) | 인프라 0, 기존 GitHub 인증 재사용. SDK/libgit2 불사용 |
| 암호화 | 전부: gzip → age. 패스프레이즈 → Argon2id → age 키. **salt는 init 시 랜덤 생성, 리포에 평문 저장**(salt는 비밀이 아님 — 고정 salt의 공통 레인보우 테이블 리스크 제거, UX 손실 0) | 세션에 민감 정보. git은 "인증된 암호화 blob 저장소"로 사용 |
| 히스토리 | 기본 유지 + `gc --squash`(운영상 필수 — §7 성장 수치), 스냅샷 모드는 config 옵션 | 롤백 보존, 비대화는 주기 관리 |
| 범위 | 아래 인벤토리 표 (allowlist + 미지 항목 감지) | "행동을 정의하는 것 전부, 머신 일시 상태 제외" |
| 경로 전략 | B+ : 스팬 스플라이싱 + 검증 하네스 (§5) | 아래 §5 |
| 레이아웃 전제 | 두 기기 홈 기준 동일 구조 | `${HOME}` 토큰 중심, `path_map`은 예외용 escape hatch |
| 사용 패턴 | 수동 push/pull, 비동시 | 가드 + 파일 분류 시맨틱(§7), merge는 `history.jsonl` 라인-유니온 특례 하나만 |

## 3. 아키텍처

```
┌─ 기기 A (Mac, woong) ──────┐      ┌─ 기기 B (Win, Loki) ───────┐
│ ~/.claude/        (평문)    │      │ C:\Users\Loki\.claude\      │
│      ↕ transform+crypto     │      │      ↕ transform+crypto     │
│ ~/.claude-xsync/repo/       │      │ ...\.claude-xsync\repo\     │
│   (암호화된 git 클론)  ─────┼──────┼──→ GitHub private repo      │
└─────────────────────────────┘      └─────────────────────────────┘
```

단일 Rust 바이너리. CLI: `init` / `push` / `pull` / `status` / `gc`. 레이어: CLI(clap) → Sync 엔진(변경감지·오케스트레이션·가드) → Transform 코어(순수) → Crypto/Transport(I/O).

### 로컬 레이아웃 (기기당)
```
~/.claude-xsync/
├── config.toml   # remote URL, device 이름, path_map, 스코프 오버라이드, 히스토리 모드
├── state.json    # 파일별 평문 sha256, 마지막 push/pull 커밋 지점
└── repo/         # 암호화 파일들의 git 클론
```

### 리모트 레이아웃
```
salt                        # 평문 랜덤 salt (init 시 생성, 키 유도용 — 비밀 아님)
manifest.age                # 암호화 인덱스: portable 경로 → {objects[], plaintext_hash, size, mode, device, ts}
objects/<HMAC(key, portable경로)>.age        # 단일 청크
objects/<HMAC(key, portable경로)>.age.0 …N   # 90MB 초과 시 청킹
```
- 오브젝트 파일명은 **유도 키로 HMAC한 경로 해시**. 순수 sha256이면 고정 경로 파일(`settings.json` 등)의 오브젝트명이 전 사용자 공통이 되어 레포 열람자가 파일 존재·변경 패턴을 식별 가능(활동 핑거프린팅) — HMAC으로 차단.
- 해시 명명의 다른 효과: (1) Windows 예약어(`con`, `aux`, `nul`…) 프로젝트명으로 인한 checkout 실패 제거, (2) 대소문자 무시 FS 충돌 제거, (3) 프로젝트명 노출 차단.
- **청킹**: 암호화 payload가 90MB를 넘으면 90MB 단위 분할(`.age.N`), manifest에 청크 수 기록. GitHub 100MB 하드 한도로 인한 "영구 싱크 불가 파일" 클래스를 제거.
- 경로↔오브젝트 매핑은 manifest 안(암호화됨). 트레이드오프: 리모트 레포는 사람이 못 읽음 — 기계 전용 레포이므로 수용. 오브젝트 수·크기·커밋 시각은 노출됨(§10).

## 4. 싱크 인벤토리

원칙: **allowlist** + push 시 allowlist에 없는 새 최상위 항목 발견하면 "싱크 안 함, 원하면 `path add`" 알림. Claude Code 업데이트로 새 폴더가 생겨도 조용히 새지 않는다.

### 기본 파이프라인 (싱크 + 경로 변환)
`projects/`(세션+auto-memory), `history.jsonl`, `file-history/`, `tasks/`, `todos/`, `plans/`, `settings.json`, `settings.local.json`, `CLAUDE.md`, `keybindings.json`, `agents/`, `skills/`, `commands/`, `rules/`, `workflows/` — 현재 없는 항목도 allowlist에 포함, 생기면 자동 편입.

### 특수 처리
- **`plugins/`**: 설치 목록/설정 파일만. `node_modules`, `.venv`, 클론 캐시 제외(아키텍처별 빌드 산출물 — 크로스-OS 복사 시 실제 파손). pull 후 Claude Code가 재설치. [실기기 검증 항목]
- **`~/.claude.json`**: 통째 싱크 금지(머신 정체성 키 70+ 실측). `mcpServers` 서브트리만 추출해 가상 파일로 싱크, pull 시 해당 키만 병합. 내부 경로는 동일 변환 적용. `projects` dict(프로젝트별 allowedTools/신뢰/MCP 활성화)는 v2 검토 — §10에 격차 명시.
- **`history.jsonl`**: both-modified 시 라인 집합 유니온 머지(append-only 특성 활용 — §7).

### 영구 제외 (머신 상태)
`ide/`, `session-env/`, **`sessions/`(실측: 실행 중 프로세스 PID 레지스트리)**, `shell-snapshots/`, `chrome/`, `cache/`, `paste-cache/`, `debug/`, `downloads/`, `backups/`, `channels/`, `stats-cache.json`, `mcp-needs-auth-cache.json`, `.last-*`, **`.credentials.json`(하드코딩 제외 — 인증은 기기별)**.

## 5. Transform 코어 (경로 정규화 규칙)

### Portable 정규형
- 구분자 항상 `/`, 매핑된 접두사는 토큰: `${HOME}`(자동), `${WORK}` 등(path_map).
- 예: `/Users/woong/workspace/foo` ↔ `${HOME}/workspace/foo` ↔ `C:\Users\Loki\workspace\foo`
- **인코딩 단사성(injectivity)**: normalize는 콘텐츠에 **원래 존재하던 `${`를 이스케이프**(예: `${` → `${ESC}`, `${ESC}` 자체는 이중화)한 뒤 토큰을 삽입한다. pull에서 역순 복원. 이로써 리터럴 `${HOME}`(셸 스크립트 논의 등)과 우리가 삽입한 토큰이 절대 혼동되지 않고, 왕복이 정확해진다.

### 홈 매칭 규칙
- **이 기기의 홈 형태만 매칭한다.** Mac 기기는 `/Users/woong`만, Windows 기기는 `C:\Users\Loki`·`C:/Users/Loki`(+JSON 이스케이프 형태)만. 상대 OS 홈 경로가 콘텐츠에 인용된 경우(로그 붙여넣기, 크로스-OS 논의)는 **의도적으로 건드리지 않는다** — 인용은 인용으로 보존되는 것이 올바른 의미론.
- **대소문자 무시 매칭, 캐논 케이스 출력.** macOS APFS·Windows NTFS 모두 case-insensitive라 `C:\users\loki`(Git Bash 소문자 진입 등) 변형이 실존 가능. 매칭은 case-insensitive, 복원 출력은 OS API(`dirs`)가 주는 캐논 형태.

### ① 디렉터리 키 (`projects/<encoded-cwd>/`)
- Claude Code 인코딩(비영숫자→`-`)된 홈을 토큰으로: `-Users-woong-…`/`C--Users-Loki-…` → `${HOME}-…`.
- **경계 규칙 필수**: `seg == encHome || seg.startsWith(encHome + "-")` — `-Users-woongho-app`이 `-Users-woong` 접두사에 오매칭되는 것을 차단(tawanorg paths.go:145와 동일 방어).
- 상속 한계: 인코딩이 손실 압축이라 홈의 형제 디렉터리(`/Users/woong.bak/…` → `-Users-woong-bak-…`)가 `${HOME}-bak-…`으로 오토큰화될 수 있음 — §10에 문서화(발생 조건: 홈 경로 + 구분자 아닌 문자로 이어지는 형제 경로를 프로젝트 루트로 사용).

### ② 파일 내용 — 스팬 스플라이싱
1. JSON/JSONL: 토크나이저가 **문자열 토큰의 바이트 범위만** 식별(키도 문자열 토큰 → `trackedFileBackups`의 절대경로-키 실측 확인, 자동 처리).
2. 각 토큰 완전 디코드(`\\`, `\uXXXX` — 한글 경로 포함).
3. 디코드 텍스트에서 위 홈 매칭 규칙 적용. 경계 인식(`/Users/woong` 뒤에 이름 문자가 오면 불일치 — `/Users/woongho` 오탐 방지).
4. 매칭 지점부터 경로 런(run) 전체를 스팬으로: 토큰화 + 구분자 `/` 정규화 + **스팬별 원본 `PathShape` 기록**(검증용 — 아래). 경로 런은 공백 미포함(§10 한계).
5. **바뀐 토큰만** 재인코드해 스플라이스. 그 외 바이트는 원본 유지 — float 재표현, 중복 키 삭제 등 재직렬화 계열 오염이 원리적으로 불가능.
- **라인 단위 fail-closed**: 파싱 불가 라인(크래시로 잘린 마지막 라인 등)은 그 라인만 무변환 통과, 정상 라인은 변환. 파일 전체 verbatim은 파일 포맷 자체가 미지일 때만.
- `.md`/`.txt`: 같은 매칭 규칙을 바이트에 직접 적용. 그 외 포맷: verbatim.
- **예외 — `file-history/` 스냅샷 blob**: undo용 파일 사본은 내용이 바이트 그대로 복원되어야 하므로 확장자와 무관하게 **항상 verbatim**(경로 변환은 메타데이터에만 적용). 내부 구조(메타데이터/blob 구분)는 구현 시 조사 항목.

### 복원 — resolve는 두 개다 (검증용 / pull용 분리)
| | 목적 | 출력 |
|---|---|---|
| **verify-resolve** | push 게이트의 바이트 왕복 검증 | 스팬별 기록된 원본 `PathShape`대로 재출력(백슬래시였으면 백슬래시로) → `verify_resolve(normalize(x)) == x` 바이트 동일 |
| **pull-resolve** | 상대 기기에서 실제 복원 | 캐논 형태 — Windows에서도 **슬래시**(`C:/Users/Loki/...`) → 복원이 백슬래시를 만들지 않아 JSON 재이스케이프 문제 소멸. 디렉터리 키 인코딩은 두 형태 동일(`C--Users-Loki`) |

이 분리가 없으면 Windows 네이티브 파일(백슬래시)이 전부 왕복 검증에 실패해 100% verbatim 강등된다(리뷰 C1). `PathShape` enum(UnixAbs/DriveBackslash/DriveSlash/Unc/Token)이 두 resolve의 공용 어휘.

- [실기기 검증 1순위] Windows에서 슬래시 cwd로 `claude --resume` + **체크포인트/rewind** 정상 동작. 실패 시 플랜B: 토크나이저 보유로 JSON 문자열 내부만 `\\` 이스케이프 복원 가능.

### resolve의 토큰 정책 (컨텍스트별 분리)
- **콘텐츠 복원**: 등록된 토큰만 치환. 미지의 `${IDENT}`(셸 `${PATH}`, TS 템플릿 등 — 실측 세션 19/20에 존재)는 **무시하고 통과**. 에러 아님.
- **dirkey/manifest 경로 복원**: 미지 토큰 = `Err(UnmappedToken)` — 해당 파일 스킵+보고 (엄격).

### 검증 하네스 (B+의 "+")
- **push 게이트**: `verify_resolve(normalized) == 원본` 바이트 왕복 검증. 실패 → verbatim 강등 + 경고. 오염 데이터는 리모트에 절대 올라가지 않음.
- **pull 게이트**: `normalize(pull_resolved)` 가 portable과 **토큰·구조 동등**(shape 무관)한지 역검증. 실패 → 해당 파일 스킵 + 보고.
- verbatim 여부는 **manifest `mode` 필드에 기록** → pull이 변환 안 된 파일에 resolve를 적용하는 사고 차단.

## 6. 모듈 구성

```
src/
├── main.rs / cli/        # clap 진입, 커맨드 오케스트레이션(로직 없음)
│  ── 순수 코어 (I/O 없음) ──
├── transform/json_spans.rs   # JSON 문자열 토큰 스팬 렉서
├── transform/pathmatch.rs    # 홈접두사+경로런 매칭·치환 (case-insensitive, shape 기록)
├── transform/dirkey.rs       # projects/ 키 인코딩·토큰화 (경계 규칙 포함)
├── mapper.rs                 # PathMapper (HOME + path_map 토큰 테이블, 캐논 케이스)
├── verify.rs                 # 왕복(verify-resolve)/역방향 검증 게이트
├── manifest.rs               # manifest 모델, HMAC 오브젝트 명명, 청킹
│  ── I/O 레이어 (얇게) ──
├── scan.rs                   # allowlist 스캔 + 미지 항목 감지
├── state.rs                  # state.json
├── crypto.rs                 # gzip→age (passphrase + 리포 salt, Argon2id 파라미터는 구현 시 명시·고정)
├── gitx.rs                   # git CLI 래퍼 (+ force-push divergence 감지)
├── fsx.rs                    # 원자적 쓰기, 백업, Windows 장경로(`\\?\`) 처리
├── procguard.rs              # 실행 중 Claude Code 감지 (sessions/*.json PID 생존 확인)
└── special/{mcp.rs, plugins.rs, history_merge.rs}
```

핵심 타입:
```rust
enum TransformOutcome { Transformed { data: Vec<u8>, shapes: SpanShapes }, Verbatim { reason: VerbatimReason } }
enum PathShape { UnixAbs, DriveBackslash, DriveSlash, Unc, Token }  // exhaustive match
// dirkey/manifest 경로: 엄격
fn resolve_key(portable: &str) -> Result<LocalPath, UnmappedToken>
// 콘텐츠: 등록 토큰만 치환, 미지 ${IDENT} 통과
fn resolve_content(text: &str, mode: ResolveMode) -> String   // ResolveMode::Verify(shapes) | ResolveMode::Pull
```

의존성: `clap`, `serde/serde_json`, `age`, `flate2`, `sha2`(+HMAC), `dirs`, `anyhow/thiserror`, `tempfile`, dev `proptest`. 제외: tokio(동기 I/O 충분), git2(shell-out), 클라우드 SDK(0개). `dirs`가 홈을 OS API로 읽음 — Git Bash MSYS 경로 변환 함정 회피(경로를 셸 인자로 받지 않는다).

## 7. 데이터 흐름

### 공통 사전 가드
- **실행 중 인스턴스 감지**: `sessions/*.json`의 PID 생존 확인 → Claude Code 실행 중이면 push/pull 중단 권고(`--force`로 무시 가능). Windows는 열린 파일 위 rename이 sharing violation으로 실패하므로 특히 pull에 필수. `.claude.json` 병합의 lost-update 경쟁도 차단.

### push
1. config·state 로드, PathMapper 구성
2. git fetch → **가드 A**: 리모트가 내 마지막 싱크보다 앞 && 마지막 push 디바이스 ≠ 나 → "상대 기기가 먼저 push함, pull 먼저" (기본 중단, `--force`)
3. allowlist 스캔(+미지 항목 알림) → 파일별 평문 해시, state 동일 시 스킵(age 비결정성 대응)
4. 변경분: normalize(shape 기록) → verify-resolve 왕복 검증(실패=verbatim) → gzip → age → `objects/` 기록(90MB 초과 시 청킹)
5. 삭제 감지 → manifest 제거. manifest 갱신(디바이스·시각 포함) → 암호화 → commit·push → state 갱신

### pull — 파일 분류 시맨틱 (가드 B를 대체)
git pull(→ force-push divergence 감지 시: repo/는 파생 데이터이므로 `fetch + reset --hard origin/main` 후 state를 manifest 기준으로 재앵커) → manifest 복호화(패스프레이즈 오류 즉시 검출) → 파일별 3-way 분류(로컬 vs state vs manifest):

| 분류 | 판정 | 동작 |
|---|---|---|
| 로컬 전용 신규 | state에 없음 && manifest에 없음 | **보존** (다음 push로 자연 합류) |
| 리모트 전용 변경 | 로컬 == state, manifest ≠ state | 적용 |
| 로컬 전용 변경 | 로컬 ≠ state, manifest == state | 보존 (push 대상) |
| **both-modified** | 로컬 ≠ state && manifest ≠ state | 리모트 적용 + 로컬본을 `<path>.xsync-conflict.<ts>`로 보존 + 보고. **`history.jsonl`은 특례: 라인 집합 유니온 머지**(append-only — 실질 손실 0) |
| 리모트 삭제 | manifest에서 사라짐 | 백업으로 이동 (hard delete 없음) |

적용 절차: 전부 스테이징 성공까지 `~/.claude` 무변경 → `~/.claude.backup.<ts>/` 백업 → 파일별 원자 쓰기(temp+rename) → mcpServers 키 병합 → state 갱신(**파일별 즉시 기록** — 중단 시 재개 정확성).

이 분류로 "A에서 push 깜빡 → B 작업·push → A 복귀" 시나리오(리뷰 C3)가 데드락 없이 해소된다: A의 pull이 로컬 전용 작업을 보존하고, 겹친 파일만 conflict 사본을 남기며, history는 유니온된다. 이후 A의 push가 나머지를 합류시킨다.

### 첫 실행
- 기기 A: `init` — remote URL·패스프레이즈·디바이스명 설정, salt 생성·커밋, 최초 push. (레포 생성은 유저가 GitHub에서)
- 기기 B: `init` — clone, salt 읽기 → 키 유도, manifest 복호화 성공=키 검증, 미리보기 후 승인 적용, 기존 `~/.claude` 통째 백업
- push/pull 공통 `--dry-run`

### gc --squash 프로토콜
1. 실행 기기: 히스토리를 단일 커밋으로 재작성 → force-push
2. 상대 기기의 다음 pull: divergence 감지 → `reset --hard origin` + state 재앵커(위 명시) — 자동, 사용자 개입 불필요
3. 권장 주기: **월 1회 또는 리포 1GB 도달 시**. 근거 실측: 활성 대형 세션(156MB→gzip ~30MB)이 변경될 때마다 push당 ~30MB 히스토리 누적(암호화 blob은 git delta 불가), 주 3회 push 가정 시 월 ~360MB.

## 8. 에러 정책

| 등급 | 예시 | 반응 | exit |
|---|---|---|---|
| 환경 오류 | git 없음, remote 불가, 패스프레이즈 오류, config 손상, Claude Code 실행 중 | 즉시 중단, 무변경, 수정 힌트 | 2 |
| 변환 이상 | 미지 포맷, 라인 파싱 실패(라인 단위), 왕복 검증 불일치 | verbatim 강등(파일/라인) + 경고, 계속 | 1 |
| 복원 이상 | dirkey/manifest 토큰 미해결, 역검증 실패 | 해당 파일 스킵 + 보고, 계속 | 1 |
| git 실패 | 네트워크/인증/non-fast-forward | stderr 노출 + 원인별 힌트 | 2 |

- 재실행 멱등: push 중단 시 `~/.claude`는 읽기만 한 상태. pull 중단 시 백업 존재 + 파일별 원자 쓰기 + **state 파일별 즉시 갱신** → 재실행이 남은 파일만 적용.
- 패스프레이즈 분실 = 리모트 복구 불가(의도된 E2E 특성). 평문은 양 기기에 존재하므로 재init로 복구. init 시 경고 문구.
- **패스프레이즈 교체 절차**: 새 키로 전체 재암호화 + 필수 squash(이전 키로 복호 가능한 히스토리 제거) — `xsync rekey` 커맨드(v1 포함, 문서화).
- 모든 커맨드 종료 시 요약 고정 출력: `✓ N synced · ⚠ M verbatim (사유) · ✗ K skipped · ⚡ C conflicts`. `--verbose`로 상세.

## 9. 테스트 · CI · 배포

### 테스트
1. **유닛(순수 코어)**: `json_spans`는 proptest + serde_json 오라클("내 렉서의 문자열들 == serde의 문자열들") — **정의역 명시**: 오라클은 serde 수용 입력에서만 정의, lone surrogate·비UTF8 등 거부 입력에서의 렉서 행동(=verbatim 강등 조건)은 별도 스펙으로 고정. `pathmatch` 테이블 테스트(Unix/`C:\`/`C:/`/UNC/한글/`\uXXXX`/대소문자 변형/경계 케이스(`woong` vs `woongho`)/공백 한계/리터럴 `${` 이스케이프 왕복). 왕복 속성 `verify_resolve(normalize(x)) == x`. dirkey 경계 규칙. mcp 추출/병합·history 유니온 멱등성.
2. **골든 픽스처**: 실제 Mac 세션 + **실제 Windows 세션(Loki 기기에서 구현 초기에 선수집** — 실기기 검증 전에 C1·케이스 계열을 조기 검출). 민감정보는 **구조 보존 자동 마스킹 스크립트**로 제거(수동 새니타이즈는 유출 리스크). 양방향 스냅샷 테스트.
3. **통합**: 로컬 bare repo + 가짜 홈 2개로 init A→push→init B→pull→동일성 assert. force-push divergence 복구, 청킹 왕복, conflict 분류 5종 각각. 3 OS CI 전부에서 실행.
4. **실기기 체크리스트(릴리스 전 수동)**: ☐ Windows 슬래시 cwd `--resume` ☐ **체크포인트/rewind (trackedFileBackups 키가 슬래시 형태일 때)** ☐ pull 후 플러그인 재설치 ☐ Git Bash/PowerShell 동일 동작 ☐ mcp 병합 후 로그인 무손상 ☐ Windows 예약어 파일명 pull 시 skip+report ☐ MAX_PATH 초과 경로(`\\?\` 프리픽스).

### CI/릴리스
- GitHub Actions 매트릭스 `{macos, windows, ubuntu}` 테스트.
- 릴리스: `darwin-arm64/x64`, `windows-x64/arm64(.exe)`, `linux-x64/arm64` 빌드 → **전부 Release 에셋에 명시** + **에셋 개수 검증 스텝**(6개 미만이면 CI 실패). tawanorg의 `.releaserc.json` 누락 지속을 구조적으로 차단.

### 배포
v1: GitHub Releases + `cargo install`. v2 후보: brew tap / Scoop. 라이선스 MIT, README에 tawanorg/claude-sync·claude-context-sync 크레딧(비교 서술은 §1 배경의 정정된 사실 기준).

## 10. 알려진 한계 (문서화할 것)
- **공백 포함 경로**: 내용 속 경로 런 매칭이 공백에서 끊김. 왕복 검증이 잡아 verbatim 강등되므로 오염은 없음. 홈 기준 동일 레이아웃 + 공백 없는 프로젝트 경로 권장.
- **상대 OS 홈 인용은 1회 흡수됨** (§12.2로 개정): 각 기기는 자기 홈 형태만 변환하므로 인용은 push 시 보존되지만, 인용된 홈의 주인 기기가 그 파일을 받으면 첫 왕복에서 살아있는 경로로 1회 흡수된다(이후 안정 번역). 원문 완전 보존이 필요하면 v2 피어 홈 레지스트리(§11).
- **좌측 경계는 구분자 예외** (§12.1로 개정): 타 루트 아래 `…/Users/<명>` 서브트리는 직전이 구분자라 여전히 번역됨(희귀 — 해당 레이아웃 비권장).
- **dirkey 손실 인코딩**: 홈의 형제 디렉터리(`/Users/woong.bak` 류)를 프로젝트 루트로 쓰면 `${HOME}-…`으로 오토큰화 가능(§5 ①). 희귀 케이스 — 해당 구성 비권장.
- **MCP·권한 격차**: 글로벌 `mcpServers`만 싱크. 프로젝트별 활성화/allowedTools/신뢰 상태(`projects` dict)는 기기마다 재구축(v2 검토). MCP `command`가 OS 종속 절대경로(`/opt/homebrew/...`)면 경로 변환으로 해결 불가 — path_map 또는 per-OS override 필요(문서 가이드 제공).
- **settings 훅·statusLine 커맨드**: 셸 커맨드 문자열은 OS별 유효성이 다를 수 있음(경로는 변환되나 실행 파일·문법 차이는 사용자 책임).
- **메타데이터 노출**: 오브젝트 수·크기·커밋 시각·salt는 리모트에서 관찰 가능(내용·이름·경로는 불가).
- **한글 파일명 NFC/NFD**: macOS(NFD)↔Windows(NFC) 정규화 차이 — 구현 시 조사, 필요 시 NFC 통일.

## 11. v2 파킹랏
`~/.claude.json`의 `projects` dict 선별 싱크(신뢰/허용도구/프로젝트별 MCP), 동기화 폴더 백엔드, brew/Scoop, SessionEnd/Start 훅 자동화(수동으로 충분해질 때까지 보류), 스냅샷 모드 기본화 검토, MCP per-OS override 문법, 피어 홈 레지스트리(인용 원문 완전 보존이 필요해지면 — §12 C′의 상위 호환).

## 12. 개정 (구현 후 적대적 리뷰 반영, 2026-07-14)

v1 구현에 대한 2차 적대적 블라인드 리뷰(Critical 2/Important 5/Minor 10)에서 §5·§10의 두 보장이 특정 입력 클래스에서 양립 불가함이 확인되어, 사용자 결정으로 아래 두 규칙을 확정한다.

### 12.1 좌측 경계 규칙 (리뷰 I2)
§5의 홈 매칭에 좌측 경계를 추가한다: **매칭 시작 직전 문자가 경로 런 문자이면 불일치. 단 구분자(`/`, `\`)는 예외로 허용.**
- 차단: `/System/Volumes/Data/Users/woong/x`(펌링크 별칭, 직전 `a`) 류의 연결 문자열이 `…Data${HOME}/x`로 오토큰화되어 상대 기기에서 괴물 문자열이 되는 것.
- 보존: `file:///Users/woong/x`(직전 `/`), `\\?\C:\Users\Loki\x`(직전 `\`)의 유용한 번역.
- 잔여 한계(§10에 병기): 타 루트 아래 `…/Users/<명>` 서브트리(`/mnt/backup/Users/woong` 등)는 직전이 구분자라 여전히 번역됨 — 2기기 개인 사용에서 수용.

### 12.2 pull 게이트의 1단계 안정성 완화 (리뷰 I1, 결정 C′)
상대 기기 홈이 **대화 텍스트에 인용**된 파일은 수신 기기에서 역검증(`normalize(resolved) == payload`)에 실패한다 — 인용이 그 기기의 살아있는 홈과 일치해 재토큰화되기 때문. §10의 "인용 보존"과 §5의 게이트는 이 클래스에서 동시에 만족 불가.
확정 의미론: **역검증 실패 시 1단계 안정성 검사로 강등한다.** `d2 = normalize(resolved)`에 대해 `resolve_pull(d2) == resolved`이면 적용(+알림), 아니면 스킵(진짜 손상 의심).
- 통과 조건이 곧 이후 왕복의 고정점 증명이므로 손상 감지력은 유지된다.
- 결과: 인용 텍스트는 **첫 왕복에서 1회** 수신 기기의 살아있는 경로로 흡수되고(예: 맥 세션 속 `C:\Users\Loki\x` 인용이 한 사이클 뒤 맥에서 `/Users/woong/x`로 보임), 이후 여느 경로처럼 안정 번역된다. 기계 소비 경로(cwd·백업 키)는 전 구간 무손상.
- §10의 "상대 OS 홈 인용은 보존됨" 항목은 "인용은 1회 흡수됨(원문 완전 보존이 필요하면 v2 피어 홈 레지스트리)"로 대체한다.
- 운영 가이드: 공용 CLAUDE.md에 멀티기기 안내문(과거 턴의 경로는 상대 기기 것일 수 있음, pwd 신뢰) 추가 권장 — README에 스니펫 제공.
