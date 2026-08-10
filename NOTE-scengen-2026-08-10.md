# scengen 담당자에게 — qc_move_log 시각 의미 관련 통보 (2026-08-10, 추출기 트랙)

scengen 코드는 규약대로 손대지 않았다. 아래는 **데이터 의미** 통보다.

## 1. 시나리오 출력의 QC `start_ts`/`service_s` 는 크레인 작업이 아니다

`assemble.rs` 의 vessel 작업목록이 `qc_move_log` 에서 `'start_ts': st_ts, 'service_s': dur_s`
를 내보내는데, **QC 쪽 st_ts 는 크레인 작업 시작이 아니라 트럭 배정 시각**이다(TOS ST_DT,
완료 시 소급 기입 — 2026-08-10 발굴조사로 확정, 4.6만 건 실측). 따라서:

- `service_s`(=dur_s=comp−st) = **트럭 배정→완료**. 양하 중앙 ~7분, **적하 중앙 ~24분**.
  크레인 서비스 시간으로 쓰면 적하가 24분짜리 무브가 된다.
- 진짜 크레인 무브 시간이 필요하면 `learn_qc_move_time`(이미 §2 로 사용 중)을 쓰는 것이 맞다.
- **landside(GI/GO) 쪽은 무관** — `rtg_move_log.st_ts` 는 진짜 물리 시작이라 그대로 옳다.
- TOS 는 QC 물리 시작을 어디에도 기록하지 않는다(MCH_OPERATION 행은 완료 시 통째 삽입,
  CYC_HISTORY 안벽 이벤트도 완료 시점 날인). 필요하면 추정식(sql/local/l_qc_q.sql 머리 참조).
- DB 컬럼 주석으로도 박아뒀다(mig0146: qc_move_log.st_ts / dur_s / rtg_move_log.st_ts).

## 2. `learn_qc_move_time` 값이 2026-08-10 부터 이동한다 (트윈 보정)

트윈(들어올림 1회·상자 2개)이 완료 0~2초 차 연속 2행으로 남아, 행 단위 간격 학습에
가짜 1~2초 표본을 만들고 있었다(전체 간격의 ~16%). 들어올림 단위로 접어 재학습하도록
고쳤고(`sql/local/l_qc_move_time.sql`), 값이 **DS 90→99s, LD 111→115s** 로 오른다.
scengen 이 이 표를 QC 서비스 시간으로 쓰고 있으므로(assemble.rs `learn_qc_move_time`
조회) 시나리오의 QC 처리 속도가 그만큼 느려진 값으로 나온다 — 보정된 쪽이 진실이다.
