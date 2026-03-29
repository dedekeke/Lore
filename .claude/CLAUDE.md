    - For every phase that has code change in the plan, always checkout a new branch target branch is develop/claude, always commit the changes, create a PR/MR for the reviewer and techlead to approve, then merge the change after being approve by both.
- condense, no blabbering, even for the comments, they are only for the key/hard to understand code block(no need comments for the easy code blocks)
- The PR #3 from develop/claude to master is for me to review, do not touch it
- don't push the .env file in any case, scenario
- always use the sqlx to create the db migration, don't manually create migration files with appended name
- remember to check locally for link, check and test to make sure the pipeline check pass before pushing and create PR


# CRITICAL OPERATING PROCEDURE: EPISODIC MEMORY

You are connected to Lore, an external memory ledger via MCP. You MUST NOT rely on your internal context window for long-running tasks. You MUST follow this exact loop:

1. **FIRST CALL**: [switch_project(name, root_path)](cci:1://file:///Users/chaunhat/workspace/personal/projects/Lore/src/server.rs:473:4-502:5) to set context. Do this at the start of every session.
2. **COLD START**: Call `get_next_steps()` to get a briefing on pending work, blocked tasks, and recent lessons — no need to resume prior context.
3. **NEW TASKS**: When the user gives you a new goal, call [start_task(description)](cci:1://file:///Users/chaunhat/workspace/personal/projects/Lore/src/server.rs:246:4-268:5) BEFORE generating any code.
4. **PROPOSING SOLUTIONS**: Before writing code, call [propose_attempt(task_id, approach, code)](cci:1://file:///Users/chaunhat/workspace/personal/projects/Lore/src/server.rs:270:4-296:5).
5. **HANDLING FAILURES**: If the user reports an error, IMMEDIATELY call [log_outcome(attempt_id, 'rejected', reasoning)](cci:1://file:///Users/chaunhat/workspace/personal/projects/Lore/src/server.rs:298:4-338:5) BEFORE suggesting a fix.
6. **DO NOT AUTO-ACCEPT**: Only call [log_outcome(attempt_id, 'accepted', reasoning)](cci:1://file:///Users/chaunhat/workspace/personal/projects/Lore/src/server.rs:298:4-338:5) when the USER explicitly confirms success. If unsure, use `'pending'`.
7. **CONTEXT RECOVERY**: If you feel lost or the user says "try something else", call [review_ledger(task_id)](cci:1://file:///Users/chaunhat/workspace/personal/projects/Lore/src/server.rs:340:4-362:5) to read past failures so you don't repeat them.
8. **PERIODIC CHECK**: Call [get_active_context()](cci:1://file:///Users/chaunhat/workspace/personal/projects/Lore/src/server.rs:431:4-471:5) every ~5 messages to stay grounded.
9. **PROTOCOL REFRESH**: If unsure what to do next, call [get_protocol()](cci:1://file:///Users/chaunhat/workspace/personal/projects/Lore/src/server.rs:544:4-547:5) to re-read these rules.
10. **TASK COMPLETION**: After user confirms success, call [complete_task(task_id, lesson)](cci:1://file:///Users/chaunhat/workspace/personal/projects/Lore/src/db/tasks.rs:96:0-104:1) to extract a lesson.
11. **CONTEXT PRESERVATION**: When context usage reaches 97%, IMMEDIATELY call `generate_handoff()` to preserve task state before context compression. Do NOT wait for the user to remind you.

Violation of these rules causes context rot and repeated failures.
