-- A synthetic claude-mem store (#1117): the real store's tables, read by
-- `memory-engine-plugin-export claude-mem`, plus `pending_messages`, which it never reads.
-- What each row becomes is the table of README.md.

-- Observation 6 names a memory session no session holds.
PRAGMA foreign_keys = OFF;

CREATE TABLE sdk_sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, content_session_id TEXT UNIQUE NOT NULL,
  memory_session_id TEXT UNIQUE, project TEXT NOT NULL, user_prompt TEXT, started_at TEXT NOT NULL,
  started_at_epoch INTEGER NOT NULL, completed_at TEXT, completed_at_epoch INTEGER,
  status TEXT CHECK(status IN ('active','completed','failed')) NOT NULL DEFAULT 'active',
  worker_port INTEGER, prompt_counter INTEGER DEFAULT 0, custom_title TEXT);
CREATE TABLE user_prompts (id INTEGER PRIMARY KEY AUTOINCREMENT, content_session_id TEXT NOT NULL,
  prompt_number INTEGER NOT NULL, prompt_text TEXT NOT NULL, created_at TEXT NOT NULL,
  created_at_epoch INTEGER NOT NULL,
  FOREIGN KEY(content_session_id) REFERENCES sdk_sessions(content_session_id) ON DELETE CASCADE);
CREATE TABLE observations (id INTEGER PRIMARY KEY AUTOINCREMENT, memory_session_id TEXT NOT NULL,
  project TEXT NOT NULL, text TEXT, type TEXT NOT NULL, title TEXT, subtitle TEXT, facts TEXT, narrative TEXT,
  concepts TEXT, files_read TEXT, files_modified TEXT, prompt_number INTEGER, discovery_tokens INTEGER DEFAULT 0,
  created_at TEXT NOT NULL, created_at_epoch INTEGER NOT NULL, content_hash TEXT,
  FOREIGN KEY(memory_session_id) REFERENCES sdk_sessions(memory_session_id) ON DELETE CASCADE ON UPDATE CASCADE);
CREATE TABLE session_summaries (id INTEGER PRIMARY KEY AUTOINCREMENT, memory_session_id TEXT NOT NULL,
  project TEXT NOT NULL, request TEXT, investigated TEXT, learned TEXT, completed TEXT, next_steps TEXT,
  files_read TEXT, files_edited TEXT, notes TEXT, prompt_number INTEGER, discovery_tokens INTEGER DEFAULT 0,
  created_at TEXT NOT NULL, created_at_epoch INTEGER NOT NULL,
  FOREIGN KEY(memory_session_id) REFERENCES sdk_sessions(memory_session_id) ON DELETE CASCADE ON UPDATE CASCADE);
CREATE TABLE pending_messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_db_id INTEGER NOT NULL,
  content_session_id TEXT NOT NULL, message_type TEXT NOT NULL CHECK(message_type IN ('observation','summarize')),
  tool_name TEXT, tool_input TEXT, tool_response TEXT, cwd TEXT, last_user_message TEXT,
  last_assistant_message TEXT, prompt_number INTEGER, status TEXT NOT NULL DEFAULT 'pending', retry_count INTEGER
  NOT NULL DEFAULT 0, created_at_epoch INTEGER NOT NULL, started_processing_at_epoch INTEGER,
  completed_at_epoch INTEGER, failed_at_epoch INTEGER);

INSERT INTO sdk_sessions (id, content_session_id, memory_session_id, project, user_prompt, started_at,
  started_at_epoch, status, prompt_counter) VALUES
  (1, 'eef5665a-2f30-497d-87e7-ef64be1076a7', 'mem-fixture-a', 'alpha', 'NEVER-EXPORTED',
   '2026-03-25T10:00:00.000Z', 1774432800000, 'failed', 2),
  (2, '31304ad9-fbf7-443f-b561-61f9cd8c2fdd', 'mem-fixture-b', 'beta', 'NEVER-EXPORTED',
   '2026-03-26T09:00:00.000Z', 1774515600000, 'failed', 2),
  (3, 'e4bff46f-24ef-4ab9-b58d-b26d091cfc13', NULL, 'alpha', 'NEVER-EXPORTED',
   '2026-03-27T08:00:00.000Z', 1774598400000, 'active', 1);

INSERT INTO user_prompts (id, content_session_id, prompt_number, prompt_text, created_at, created_at_epoch) VALUES
  (1, 'eef5665a-2f30-497d-87e7-ef64be1076a7', 1, 'Add a retry flag to the fetch command.',
   '2026-03-25T10:00:00.120Z', 1774432800120),
  (2, 'eef5665a-2f30-497d-87e7-ef64be1076a7', 2, 'Now document the flag.',
   '2026-03-25T10:00:00.120Z', 1774432800120),
  (3, '31304ad9-fbf7-443f-b561-61f9cd8c2fdd', 1, 'Should the retry count keep its default?',
   '2026-03-26T09:00:05.000Z', 1774515605000),
  (4, '31304ad9-fbf7-443f-b561-61f9cd8c2fdd', 2, '',
   '2026-03-26T09:10:00.000Z', 1774516200000),
  (5, 'e4bff46f-24ef-4ab9-b58d-b26d091cfc13', 1, 'What does the retry flag default to?',
   '2026-03-27T08:00:01.000Z', 1774598401000);

INSERT INTO observations (id, memory_session_id, project, text, type, title, subtitle, facts, narrative, concepts,
  files_read, files_modified, prompt_number, discovery_tokens, created_at, created_at_epoch, content_hash) VALUES
  (1, 'mem-fixture-a', 'alpha', NULL, 'discovery', 'Retry flag parsing', 'How the fetch command reads its flags',
   '["The flags are parsed once, at start.","The retry count defaults to three."]',
   'The fetch command reads every flag before its first request.', '["how-it-works","pattern"]',
   '["NEVER-EXPORTED"]', '[]', 1, 120, '2026-03-25T10:01:00.000Z', 1774432860000, 'NEVER-EXPORTED'),
  (2, 'mem-fixture-a', 'alpha', NULL, 'change', 'Retry flag added', NULL, '["fetch takes a retry flag."]',
   'Added the flag and its help line.', '["what-changed"]',
   '[]', '[]', 1, 80, '2026-03-25T10:02:00.000Z', 1774432920000, NULL),
  (3, 'mem-fixture-a', 'alpha', NULL, 'discovery', NULL, NULL, '[]', NULL, '[]',
   '[]', '[]', 2, 0, '2026-03-25T10:03:00.000Z', 1774432980000, NULL),
  (4, 'mem-fixture-a', 'alpha', NULL, 'bugfix', 'Timeout raised', NULL, 'not a list',
   'The second try waited too little.', '["gotcha"]',
   '[]', '[]', 2, 60, '2026-03-25T10:04:00.000Z', 1774433040000, NULL),
  (5, 'mem-fixture-b', 'beta', NULL, 'decision', 'Keep the default', 'Three tries stay the default', '[]',
   'Changing it would surprise the scripts that call fetch.', 'pattern',
   '[]', '[]', 1, 40, '2026-03-26T09:01:00.000Z', 1774515660000, NULL),
  (6, 'mem-fixture-gone', 'beta', NULL, 'change', 'An orphan', NULL, '[]', 'Its session is gone.', '[]',
   '[]', '[]', 1, 10, '2026-03-26T09:02:00.000Z', 1774515720000, NULL);

INSERT INTO session_summaries (id, memory_session_id, project, request, investigated, learned, completed, next_steps,
  files_read, files_edited, notes, prompt_number, discovery_tokens, created_at, created_at_epoch) VALUES
  (1, 'mem-fixture-a', 'alpha', 'Add a retry flag.', 'How fetch parses its flags.', 'Flags are read once.',
   'The flag and its help line.', 'Document the flag.', NULL, NULL, 'The default stays three.', 1, 200,
   '2026-03-25T10:05:00.000Z', 1774433100000),
  (2, 'mem-fixture-b', 'beta', 'Decide the default.', '', '', 'Kept three tries.', '', NULL, NULL, '', 1, 50,
   '2026-03-26T09:05:00.000Z', 1774515900000),
  (3, 'mem-fixture-a', 'alpha', '', '', '', '', '', NULL, NULL, '', 2, 0,
   '2026-03-25T10:06:00.000Z', 1774433160000);

INSERT INTO pending_messages (id, session_db_id, content_session_id, message_type, tool_name, tool_input,
  tool_response, cwd, last_user_message, last_assistant_message, prompt_number, status, created_at_epoch) VALUES
  (1, 1, 'eef5665a-2f30-497d-87e7-ef64be1076a7', 'observation', 'Bash', 'NEVER-EXPORTED', 'NEVER-EXPORTED',
   'NEVER-EXPORTED', 'NEVER-EXPORTED', 'NEVER-EXPORTED', 1, 'failed', 1774432830000);
