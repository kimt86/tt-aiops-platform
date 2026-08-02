-- 0112: 아무도 읽지 않는 225m 격자 매뷰와 그 조회 함수를 제거한다.
--
-- 무엇이 죽어 있었나 (2026-08-03 확인):
--   `learn_travel_zone225` 는 5분마다 REFRESH MATERIALIZED VIEW CONCURRENTLY 로 갱신되는데
--   (crates/api/src/livemap.rs 의 spawn_travel_aggregator), **읽는 코드가 한 줄도 없다.**
--   · 저장소 전 범위 grep: 코드 참조는 REFRESH 문과 낡은 주석뿐
--   · DB 의존(뷰/매뷰): 0
--   · 유일한 DB 참조자는 함수 travel_cost_lookup(mig 0051 → 0054) 인데, 그 함수를 호출하는
--     코드도 없다(livemap.rs:4119 의 주석 한 줄이 전부).
--   비용: 갱신 1회당 2.26~2.29초(실측 로그) × 5분 주기 = 하루 약 11분의 DB 작업 + 68MB.
--
-- 왜 지금 지우나:
--   배차 이동비용은 2026-07-02(mig 0082)부터 추론 도로망 경로시간 곡선(roadgraph.rs RouteCost)
--   에서 나온다. 격자 룩업은 그때 대체됐고 KC 문서도 이미 "참조용(비용 미사용)"이라 적고 있다.
--   그런데 코드 주석은 "dispatch cost now uses REALIZED learn_travel_zone225" 라고 서로 모순되게
--   남아 있어, 읽는 사람마다 "이게 비용에 쓰이나?"를 다시 확인하는 비용이 계속 들었다.
--   갱신을 멈추면서 매뷰만 남기면 **낡은 데이터가 살아있는 척** 하므로 더 나쁘다 — 같이 지운다.
--
-- 되돌리려면: mig 0051(매뷰 + 함수 v1) → mig 0054(함수 v2, 맨해튼 L3 추가) 순서로 다시 실행.
--   두 파일이 그대로 남아 있으므로 복구는 정의를 다시 붙여넣는 일이지 재작성이 아니다.
--
-- 멱등: IF EXISTS.

DROP FUNCTION IF EXISTS travel_cost_lookup(float8, float8, float8, float8);
DROP MATERIALIZED VIEW IF EXISTS learn_travel_zone225;
