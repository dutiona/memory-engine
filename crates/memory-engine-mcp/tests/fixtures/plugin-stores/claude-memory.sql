-- A synthetic claude-memory store (#1117): the real store's tables, read by
-- `memory-engine-plugin-export claude-memory`, plus `branches`, `branch_messages` and `import_log`,
-- which it never reads. What each row becomes is the table of README.md.

CREATE TABLE projects (id INTEGER PRIMARY KEY, path TEXT UNIQUE NOT NULL, key TEXT UNIQUE NOT NULL, name TEXT,
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE sessions (id INTEGER PRIMARY KEY, uuid TEXT UNIQUE NOT NULL, project_id INTEGER REFERENCES projects(id),
  parent_session_id INTEGER REFERENCES sessions(id), git_branch TEXT, cwd TEXT,
  imported_at DATETIME DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id INTEGER NOT NULL REFERENCES sessions(id), uuid TEXT,
  parent_uuid TEXT, timestamp DATETIME, role TEXT CHECK(role IN ('user','assistant')), content TEXT NOT NULL,
  tool_summary TEXT, has_tool_use INTEGER DEFAULT 0, has_thinking INTEGER DEFAULT 0,
  is_notification INTEGER DEFAULT 0, UNIQUE(session_id, uuid));
CREATE TABLE branches (id INTEGER PRIMARY KEY, session_id INTEGER NOT NULL REFERENCES sessions(id),
  leaf_uuid TEXT NOT NULL, fork_point_uuid TEXT, is_active INTEGER DEFAULT 1, started_at DATETIME, ended_at DATETIME,
  exchange_count INTEGER DEFAULT 0, files_modified TEXT, commits TEXT, aggregated_content TEXT,
  UNIQUE(session_id, leaf_uuid));
CREATE TABLE branch_messages (branch_id INTEGER NOT NULL REFERENCES branches(id),
  message_id INTEGER NOT NULL REFERENCES messages(id), PRIMARY KEY (branch_id, message_id));
CREATE TABLE import_log (id INTEGER PRIMARY KEY, file_path TEXT UNIQUE NOT NULL, file_hash TEXT,
  imported_at DATETIME DEFAULT CURRENT_TIMESTAMP, messages_imported INTEGER DEFAULT 0);

INSERT INTO projects (id, path, key, name, created_at) VALUES
  (1, '/work/fixture/alpha', 'NEVER-EXPORTED-alpha', 'NEVER-EXPORTED', '2026-03-25 10:00:00'),
  (2, '/work/fixture/beta', 'NEVER-EXPORTED-beta', 'NEVER-EXPORTED', '2026-03-28 14:00:00');

INSERT INTO sessions (id, uuid, project_id, parent_session_id, git_branch, cwd, imported_at) VALUES
  (1, 'eef5665a-2f30-497d-87e7-ef64be1076a7', 1, NULL, 'NEVER-EXPORTED', '/work/fixture/alpha', '2026-04-01 00:00:00'),
  (2, 'b14d1330-d7c4-4979-9fd6-1cbfad0113eb', 2, NULL, 'NEVER-EXPORTED', '/work/fixture/beta', '2026-04-01 00:00:00'),
  (3, 'a5b6c7d', 1, 1, 'NEVER-EXPORTED', '/work/fixture/alpha', '2026-04-01 00:00:00'),
  (4, '6abf0af7-d5d4-4183-97b4-14142808dffb', NULL, NULL, 'NEVER-EXPORTED', '/work/fixture/gamma', '2026-04-01 00:00:00');

INSERT INTO messages (id, session_id, uuid, parent_uuid, timestamp, role, content, tool_summary, has_tool_use,
  has_thinking, is_notification) VALUES
  (1, 1, 'f373ecaf-6949-4288-b8f8-3a3f1bd1a606', NULL, '2026-03-25T10:00:00.120Z', 'user',
   'Add a retry flag to the fetch command.', NULL, 0, 0, 0),
  (2, 1, 'f6f1831b-33c7-41b5-b5ab-e113cc5f18b5', 'f373ecaf-6949-4288-b8f8-3a3f1bd1a606', '2026-03-25T10:00:10.000Z',
   'assistant', 'Looking at the fetch command first.', '{"Read": 1}', 1, 1, 0),
  (3, 1, '4fc05562-f4d7-431a-b7c5-294772f947ea', 'f6f1831b-33c7-41b5-b5ab-e113cc5f18b5', '2026-03-25T10:00:30.000Z',
   'assistant', 'The fetch command now takes a retry flag.', NULL, 0, 0, 0),
  (4, 1, '1ac49302-e8e3-4f08-b00a-1051ce39fe6e', '4fc05562-f4d7-431a-b7c5-294772f947ea', '2026-03-25T10:01:00.000Z',
   'user', '<task-notification>The build finished.</task-notification>', NULL, 0, 0, 1),
  (5, 1, '9656039c-5e5e-4470-93ad-5ad61f35f36d', '1ac49302-e8e3-4f08-b00a-1051ce39fe6e', '2026-03-25T10:01:05.000Z',
   'assistant', 'The build finished cleanly.', NULL, 0, 0, 0),
  (6, 1, '214eba6d-a9b8-4574-ba6a-615a0bef5386', '9656039c-5e5e-4470-93ad-5ad61f35f36d', '2026-03-25T10:02:00.000Z',
   'user', 'Now document the flag.', NULL, 0, 0, 0),
  (7, 1, '87edc020-8c11-451f-9fea-9ca7b7bb613a', '214eba6d-a9b8-4574-ba6a-615a0bef5386', '2026-03-25T10:02:10.000Z',
   'assistant', 'Writing the docs section.', NULL, 0, 0, 0),
  (8, 1, '2d0ebe80-7198-4ef3-bb7f-673f6c317d8b', '87edc020-8c11-451f-9fea-9ca7b7bb613a', '2026-03-25T10:02:20.000Z',
   'user', '[Request interrupted by user]', NULL, 0, 0, 0),
  (9, 1, '75d52698-0cbe-4d0f-934f-82d6bc1c1490', '2d0ebe80-7198-4ef3-bb7f-673f6c317d8b', '2026-03-25T10:02:25.000Z',
   'assistant', 'Stopped. Waiting for directions.', NULL, 0, 0, 0),
  (10, 1, '9b153fff-d2bd-4c8b-81ef-90d92fd8801a', '75d52698-0cbe-4d0f-934f-82d6bc1c1490', '2026-03-25T10:03:00.000Z',
   'user', 'Keep the section short.', NULL, 0, 0, 0),
  (11, 1, '02ad0bec-768d-4faf-b5c9-6106aecd0c43', '9b153fff-d2bd-4c8b-81ef-90d92fd8801a', '2026-03-25T10:03:05.000Z',
   'user', 'Also add an example.', NULL, 0, 0, 0),
  (12, 1, 'fa947f46-b323-4417-b2ca-9cd2e21935da', '02ad0bec-768d-4faf-b5c9-6106aecd0c43', '2026-03-25T10:03:30.000Z',
   'assistant', 'Added a short section.', NULL, 0, 0, 0),
  (13, 1, '27cbe3e2-e989-4825-ab75-44d0420dc8e9', 'fa947f46-b323-4417-b2ca-9cd2e21935da', '2026-03-25T10:03:30.000Z',
   'assistant', 'Added a short section with an example.', NULL, 0, 0, 0),
  (20, 3, '3e3cec8b-9cd4-4c8d-a383-e6c49b0c2b55', NULL, '2026-03-25T10:00:40.000Z', 'user',
   'Find every caller of fetch.', NULL, 0, 0, 0),
  (21, 3, '87537b77-d84e-4efc-9a2c-b4eab2c30672', '3e3cec8b-9cd4-4c8d-a383-e6c49b0c2b55', '2026-03-25T10:00:50.000Z',
   'assistant', 'Three callers found.', NULL, 0, 0, 0),
  (22, 3, '2a30e5fe-b2c0-468f-a97d-e66832a8c673', '87537b77-d84e-4efc-9a2c-b4eab2c30672', '2026-03-25T10:00:55.000Z',
   'user', 'List their files.', NULL, 0, 0, 0),
  (23, 3, 'cdc6c2ea-b5d2-483a-a04b-8f681c78a555', '2a30e5fe-b2c0-468f-a97d-e66832a8c673', '2026-03-25T10:00:58.000Z',
   'assistant', 'They sit in the cli module.', NULL, 0, 0, 0),
  (30, 2, '53a9e0f6-a1d7-4ea3-afa0-3e7adc2a5bc4', NULL, '2026-03-28T14:00:00.000Z', 'assistant',
   'Resuming from the earlier summary.', NULL, 0, 0, 0),
  (31, 2, '35678402-f9b7-4ffa-9eef-f4e8d526cbc4', '53a9e0f6-a1d7-4ea3-afa0-3e7adc2a5bc4', '2026-03-28T14:00:30.000Z',
   'user', 'Run the tests again.', NULL, 0, 0, 0),
  (32, 2, '247bb874-098a-4874-a6bc-8da4a82dc550', '35678402-f9b7-4ffa-9eef-f4e8d526cbc4', '2026-03-28T14:01:00.000Z',
   'assistant', 'All tests pass.', NULL, 0, 0, 0),
  (40, 4, '25e03919-c9d8-4205-af95-abb995f0be64', NULL, '2026-03-29T16:00:00.000Z', 'user',
   'What does the retry flag default to?', NULL, 0, 0, 0),
  (41, 4, '03b47432-28de-475d-8f4a-4ba489eaefe2', '25e03919-c9d8-4205-af95-abb995f0be64', '2026-03-29T16:00:10.000Z',
   'user', '', NULL, 0, 0, 0),
  (42, 4, '67a66d8a-29c2-458d-a433-7db294fb8843', '03b47432-28de-475d-8f4a-4ba489eaefe2', 'not a time',
   'user', 'Is three enough?', NULL, 0, 0, 0);

INSERT INTO branches (id, session_id, leaf_uuid, fork_point_uuid, is_active, started_at, ended_at, exchange_count,
  files_modified, commits, aggregated_content) VALUES
  (1, 1, '27cbe3e2-e989-4825-ab75-44d0420dc8e9', NULL, 1, '2026-03-25T10:00:00.120Z', '2026-03-25T10:03:30.000Z', 6,
   '[]', '[]', 'NEVER-EXPORTED');
INSERT INTO branch_messages (branch_id, message_id) VALUES
  (1, 1), (1, 2), (1, 3), (1, 4), (1, 5), (1, 6), (1, 7), (1, 8), (1, 9), (1, 10), (1, 11), (1, 12), (1, 13);

INSERT INTO import_log (id, file_path, file_hash, imported_at, messages_imported) VALUES
  (1, 'NEVER-EXPORTED', 'fixture', '2026-04-01 00:00:00', 13);
