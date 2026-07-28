-- 0107: 운영 경고를 사람에게 도달시키기 위한 영속 저장소.
--
-- 왜 필요한가 (2026-07-28 적대 감사 확증):
--   `db::prune` 실패와 `size_watchdog` 문턱 초과는 `tracing::warn!` 한 줄로 끝난다. 저장소 전체에
--   외부 알림 연동이 0건이라(slack/webhook/pagerduty/smtp 검색 히트는 전부 `slack_s` = 배차 마감
--   여유 오탐) 그 경고를 사람이 받을 방법이 없다. 즉 앞선 수정은 "조용한 실패"를 "아무도 안 읽는
--   로그"로 바꾼 것에 불과했다. 실측 사례: road_route_eval 이 07-17·18·19 사흘간 0행이었는데
--   알림 0건 / 도로망 총연장이 나흘간 오염 구간에 머물렀는데 알림 0건.
--
-- 설계 결정 3가지:
--  1) PK = (source, subject) 업서트. 같은 조건이 반복돼도 행이 늘지 않고 occurrences 만 오른다.
--     경고 테이블 자체가 무한히 자라면 그것이 바로 이 프로젝트가 방금 겪은 실패 유형이다.
--  2) ack 컬럼이 없다. 대신 **자가 해소**: 소비자는 last_ts 가 최근인 행만 보여준다. 조건이
--     사라지면 갱신이 멈추고 배너에서 저절로 내려간다. 수동 ack 는 필연적으로 방치되고, 방치된
--     crit 은 영구 배너가 되어 경보 피로로 알림 배선을 다시 무력화한다.
--  3) 행 수 하드 캡 트리거. dedupe 키에 타임스탬프·차량번호·좌표를 넣는 미래 호출자 하나로
--     이 테이블이 폭주하는 것을 막는다 — 캡에 걸린 사실 자체도 알림으로 남는다.
--
-- 멱등: db/apply.sh 가 전체 파일을 매번 실행한다.

CREATE TABLE IF NOT EXISTS ops_alert (
  source      text        NOT NULL,        -- 누가 올렸나: retention_prune · size_watchdog · deadman · unit_failure
  subject     text        NOT NULL,        -- 무엇에 대한 것인가: 테이블명 · 스트림명 · 유닛명
  severity    text        NOT NULL,
  message     text        NOT NULL,
  detail      text,
  first_ts    timestamptz NOT NULL DEFAULT now(),
  last_ts     timestamptz NOT NULL DEFAULT now(),
  occurrences bigint      NOT NULL DEFAULT 1,
  PRIMARY KEY (source, subject)
);

DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ops_alert_severity_check') THEN
    ALTER TABLE ops_alert ADD CONSTRAINT ops_alert_severity_check
      CHECK (severity IN ('warn', 'crit'));
  END IF;
END $$;

CREATE INDEX IF NOT EXISTS ops_alert_last_ts_idx ON ops_alert (last_ts DESC);

-- 행 수 물리 상한. 캡을 넘으면 새 KEY 는 버리고(기존 키 갱신은 계속 통과) 캡에 걸렸다는 사실을
-- 알림으로 남긴다. source='ops_alert' 은 검사에서 제외한다 — 그러지 않으면 캡 알림 자체가
-- 트리거를 재귀 호출한다.
CREATE OR REPLACE FUNCTION ops_alert_cap() RETURNS trigger AS $fn$
DECLARE
  n bigint;
BEGIN
  IF NEW.source = 'ops_alert' THEN
    RETURN NEW;
  END IF;
  -- ⚠ 이미 있는 키는 검사에서 제외한다. BEFORE INSERT 트리거는 ON CONFLICT 해소보다 먼저
  -- 발화하므로, 이 조건이 없으면 캡에 도달한 순간 "갱신이 될 예정인 INSERT"까지 전부 버려진다.
  -- 그러면 last_ts 가 멈추고, 자가 해소 설계상 소비자는 last_ts 가 낡은 알림을 숨기므로
  -- 경고가 가장 많은 순간에 배너가 텅 비게 된다(실측으로 잡았다: 캡 후 occurrences 3->3).
  IF EXISTS (SELECT 1 FROM ops_alert WHERE source = NEW.source AND subject = NEW.subject) THEN
    RETURN NEW;
  END IF;
  SELECT count(*) INTO n FROM ops_alert;
  IF n >= 500 THEN
    INSERT INTO ops_alert (source, subject, severity, message, detail)
    VALUES ('ops_alert', 'cap', 'crit',
            'ops_alert 행 상한(500) 도달 — 새 알림 키가 버려지고 있다',
            'dedupe 키에 가변값(타임스탬프·차량번호·좌표)을 넣은 호출자를 찾아라')
    ON CONFLICT (source, subject) DO UPDATE
      SET last_ts = now(), occurrences = ops_alert.occurrences + 1;
    RETURN NULL;
  END IF;
  RETURN NEW;
END
$fn$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS ops_alert_cap_trg ON ops_alert;
CREATE TRIGGER ops_alert_cap_trg BEFORE INSERT ON ops_alert
  FOR EACH ROW EXECUTE FUNCTION ops_alert_cap();
