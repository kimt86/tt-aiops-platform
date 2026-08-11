# HANDOFF — 배차 서비스 실배차 전환

마지막 갱신 2026-08-11. 앞선 사이클: 라이브 마무리 9건(커밋 `094a274`~`326f5cb`, main 에 푸시됨).

> 상세 기준선·함정은 `~/.claude/notes/tt-aiops-platform.md` 2026-08-11 절에 있다.
> 이 파일은 **다음에 무엇을 할지**만 담는다.

---

## DONE CRITERIA (작업을 끝냈다고 말하기 전에 전부 돌릴 것)

```bash
cargo build --release -p tt-api && cargo build --release -p tt-extractor
cargo test --workspace                      # 59 통과 · 실패 0 이 기준
systemctl --user is-active tt-api           # active
```
```sql
-- 배차 파이프라인이 살아 있는가 (틱 60/시간 · 경보 0 · 작업목록 12~62초)
SELECT EXTRACT(epoch FROM now()-max(ts))::int AS 마지막틱_초전,
       (SELECT count(*) FROM stage2_solver_shadow WHERE ts > now()-interval '1 hour') AS 최근1h_틱,
       (SELECT count(*) FROM ops_alert WHERE last_ts > now()-interval '3 hours') AS 최근3h_경보,
       (SELECT EXTRACT(epoch FROM now()-last_success_at)::int
          FROM data_freshness WHERE kpi_key='WORKPOOL') AS 작업목록_나이초
  FROM stage2_solver_shadow;
```
```bash
# 유닛 드리프트 0 (저장소 == 설치본)
for f in deploy/systemd/*.service deploy/systemd/*.timer; do
  b=$(basename "$f"); i="$HOME/.config/systemd/user/$b"
  [ -f "$i" ] || { echo "미설치: $b"; continue; }
  diff -q "$f" "$i" >/dev/null || echo "차이: $b"
done
```
2026-08-11 실측: 59/0 · active · 마지막 틱 29초 전 · 60틱/시간 · 경보 0 · 작업목록 58초 · 드리프트 0.

---

## 지금 참인 것 (전에는 아니었던 것)

- **낡은 작업목록으로는 추천이 나가지 않는다.** 임계 300초, 신선도 출처는
  `data_freshness(kpi_key='WORKPOOL')`. 타이머 6분 정지 드릴로 차단·경보·복구 3단 실측.
- **일회성 추출 유닛 16개가 실패하면 경보가 뜬다**(warn). scengen 10개는 담당 분리로 제외.
- **배정 알고리즘에 회귀 테스트 7건**이 있고, 돌연변이 3종으로 실효성을 확인했다.
- **정책 2건이 결정으로 확정**됐다: 늦는 추천도 발신 · 특별취급(냉동·위험물·OOG) 미제외.
- **active 전환에 남은 코드 작업이 없다.** 유닛의 `DISPATCH_MODE=shadow` → `active` 하나뿐.
- KC 런치플랜이 현행이고(D2 완료 반영), TOS 협의 자료 `docs/tos-integration-handoff.md` 가 있다.

## 일부러 안 한 것

- **`DISPATCH_MODE=active` 로 켜지 않았다.** 켜는 순간의 유일한 효과는 "우리가 직전 180초에
  추천한 상자를 풀에서 뺀다"인데, 이건 TOS 가 우리 추천을 실제로 소비할 때 의미가 생긴다.
  소비 채널이 없는 지금 켜면 그림자 커버리지만 줄고 얻는 것이 없다.
- **`POOL_MARGIN_S`(300초) 조정 안 함.** 순번 기울기 수정 → 재측정 → 그다음이 마진이라는
  순서를 2026-08-07 에 정해뒀다. IQR ~40분이 마진 스케일보다 훨씬 크다.
- **예측 정확도 추가 개선 안 함.** 2026-08-10 결정으로 마감은 출항 페이스 기준이 됐고
  채점은 계기로만 남는다. 라이브 차단 요소가 아니다.
- **`scripts/*.sh` 의 평문 `PGPASSWORD=wp` 안 건드림.** 범위 밖이었다(아래 후보 참조).

## 이번 사이클에서 나온 범위 밖 발견

- `data_freshness` 에 `WORKPOOL_DELTA`·`WORKQUEUE_DELTA`·`*_RECON` 행이 남아 있다.
  CHUNK 5 철회 잔재로 보이나 **미확인**. 무해하지만 신선도 표를 읽을 때 헷갈린다.
- `.claude/` 가 untracked 다. gitignore 에 넣을지 커밋할지 미결.
- `kc-small` CSS 클래스가 정의 없이 문서에서 쓰인다(현재 2곳). 효과가 없다.
- `web/public/livemap-roadgraph.geojson` 은 매시 크론이 다시 써서 항상 modified 로 뜬다.
  주기적으로 커밋하는 것이 관례이나 자동화돼 있지 않다.
- 기존 경보 2건이 미해소 상태로 남아 있다: `disk/filesystem`(2026-08-09, root 권한자 몫),
  `deadman/road_route_eval`(2026-08-03).

## 다음 후보 (한 줄 근거)

1. **TOS 기술 세션 잡기** — `docs/tos-integration-handoff.md` 의 7개 질문이 답을 받아야
   견적·일정이 나온다. 기술 쪽은 우리가 더 할 게 없어서 이게 유일한 크리티컬 패스다.
2. **운영자 채택/기각 기록 장치** — 파일럿 단계(Phase 1→2) 통과 근거를 만들 유일한 방법.
   지금 있는 "TOS 가 결과적으로 같은 상자를 골랐나"는 사람의 판단이 아니다.
3. **평문 비밀번호 정리** — 원격이 있는 저장소에 `PGPASSWORD=wp` 가 커밋돼 있다.
   `.env` 참조로 바꾸면 되고, 이관·보안 검토 전에 하는 편이 싸다.
4. **`scripts/unit_failed_alert.sh` 를 scengen 유닛에도** — 같은 훅 한 줄이면 되지만
   담당이 달라 이번엔 안 건드렸다. 그쪽 에이전트와 합의 필요.
5. **디스크 root 영역** — 2026-08-09 에 실제로 여유 20GiB 밑까지 갔다. 우리 몫은 유한함을
   확인했고 나머지는 root 권한자 몫이라 이관 필요. 실배차 전 확인 권고.

## 사용자가 답해야 하는 것

- **`DISPATCH_MODE=active` 를 언제 켤 것인가?** 위 "일부러 안 한 것" 참조 — TOS 소비 채널이
  생기는 시점과 묶는 것이 맞다고 보는데, 먼저 켜서 게이지를 보고 싶다면 그것도 가능하다.
- **TOS 에 "냉동·위험물·OOG 배차에 사람이 지키는 절차가 있는가"를 물을 것인가?**
  있다면 "빼지 않는다" 결정을 뒤집어야 한다. 우리 자료로는 확인 불가.
- **`.claude/` 를 저장소에 넣을 것인가?** (agents/·settings.local.json 이 들어 있다)
- **평문 비밀번호 정리를 지금 할 것인가**, 이관 시점으로 미룰 것인가?
