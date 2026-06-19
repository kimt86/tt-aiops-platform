-- Non-TT-confound-corrected QC starvation: GPS-distance starvation gated on the crane actually
-- having pending work (live_workqueue total>comp). Removes no-work idle from the signal; transient
-- hatch-cover / bay-move gaps are further damped by the live rolling average. starving_real ≤
-- starving_gps. wait_real_s = avg idle seconds among the real-starving cranes that tick.
ALTER TABLE qc_wait_sample ADD COLUMN IF NOT EXISTS starving_real int;
ALTER TABLE qc_wait_sample ADD COLUMN IF NOT EXISTS wait_real_s   int;
