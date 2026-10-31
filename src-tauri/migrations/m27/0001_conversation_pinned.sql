-- 会话置顶:只影响列表排序,不改动消息与快照。
ALTER TABLE conversation ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0 CHECK(pinned IN (0,1));
