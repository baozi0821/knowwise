-- 书签备注:与 label(名称)分开,便于“位置 + 说明”而不改变排版。
ALTER TABLE bookmark ADD COLUMN note TEXT NOT NULL DEFAULT '';
