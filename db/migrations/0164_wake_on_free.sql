-- 0164 — 매칭이 자유 사건 착지에도 깨어난다 (2026-10-08)
--
-- 배경: 빈 트럭을 우리가 알기까지(트럭이 빈 순간 → 매칭이 그 트럭을 처음 볼 수 있는 틱) 중앙
--   양하 44초 · 적하 85초였다(10-06 03:51:50Z~10-07 23:14:10Z, 자유 사건 35,629건,
--   scripts/free_to_reco_latency.sql). 그중 ① 수집 대기 ~32초(1분 폴링) · ② 매칭이 작업목록 착지(1분)에만
--   깨어나서 생기는 대기 양하 13초 · 적하 53초. 연계 후 TOS 는 우리가 안 보낸 빈 트럭을 그냥 두므로 이
--   시간이 곧 트럭이 노는 시간이다.
-- 변경: ② 매칭이 새 자유 사건(적하 qc_move_log LD · 양하 tos_handover_label DS 의 captured_at 전진)에도
--   깨어난다 — wake_src='free'. ① 두 수집 유닛 주기 60→15초는 유닛 파일 쪽(같은 사이클).
--   킬스위치: tt-api 환경변수 MATCH_WAKE_ON_FREE=0.
-- 스키마 변경 없음 — 컬럼 의미(COMMENT)만 갱신한다. 멱등.

COMMENT ON COLUMN stage2_solver_shadow.wake_src IS
  '매칭 틱이 깨어난 이유. landing=작업목록 착지 신호(data_freshness(WORKPOOL).last_success_at 전진) / '
  'free=새 자유 사건 착지(적하 qc_move_log LD·양하 tos_handover_label DS 의 captured_at 전진, 2026-10-08 mig0164~ · 목록은 직전 틱과 같다) / '
  'fallback=최대 대기 150초(하트비트) 소진(목록 그대로) / startup=프로세스 기동 직후 첫 회(나이가 임의라 landing 에 섞지 말 것) / '
  'NULL=2026-08-12 경계 이전(고정 위상 :15). '
  'workpool_age_s 를 집계할 때는 반드시 이 컬럼으로 먼저 가른다 — 나이로 이유를 추정하면 동어반복이다. '
  '⚠ mig0164 부터 틱이 분당 1회가 아니다(자유 착지마다 추가) — 틱당·틱수 지표는 wake_src 로 가르거나 시간당으로 낼 것.';

COMMENT ON COLUMN stage2_solver_shadow.workpool_age_s IS
  '매칭이 쓴 작업목록의 **착지 이후 경과**(초) = now() - data_freshness(WORKPOOL).last_success_at. '
  '⚠ 목록이 담은 터미널 상태의 나이가 아니다 — 추출기는 Oracle 조회 전에 as_of 를 찍고 조회에 '
  '평균 ~20초가 들므로 내용의 나이는 이 값보다 ~20초 많다. '
  '대역 경계: NULL=2026-08-11 mig0150 이전(미기록) / 6~15초=고정 위상 :15 구간(2026-08-11~08-12) / '
  '0~3초=착지 신호로 깨우는 구간(2026-08-12~, wake_src=landing · fallback 은 150초 이상이 정상 · '
  'free 는 0~60초가 정상(직전 목록을 그대로 쓴다), mig0164~). '
  '⚠ mig0150 머리말의 "MATCH_TICK_SEC 만 바꾸고 타이머를 안 맞춘다" 는 낡은 서술이다 — '
  'MATCH_TICK_SEC 도 tt-workpool.timer 와의 짝 관계도 이 사이클에 사라졌다.';
