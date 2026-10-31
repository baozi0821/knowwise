-- M06 标注/待澄清备注:备注与标注内容、问题正文分开存储。
-- 涂鸦笔迹、文本框坐标、高亮/下划线样式仍在 note_text 中,编辑备注绝不改写它。
ALTER TABLE annotation ADD COLUMN remark TEXT NOT NULL DEFAULT '';
ALTER TABLE clarification_item ADD COLUMN remark TEXT NOT NULL DEFAULT '';
