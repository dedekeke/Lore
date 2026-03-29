- For every phase that has code change in the plan, always checkout a new branch target branch is develop/claude, always commit the changes, create a PR/MR for the reviewer and techlead to approve, then merge the change after being approve by both.
- condense, no blabbering, even for the comments, they are only for the key/hard to understand code block(no need comments for the easy code blocks)
- The PR #3 from develop/claude to master is for me to review, do not touch it
- don't push the .env file in any case, scenario
- always use the sqlx to create the db migration, don't manually create migration files with appended name
- remember to check locally for link, check and test to make sure the pipeline check pass before pushing and create PR


# CRITICAL OPERATING PROCEDURE: EPISODIC MEMORY

You are connected to Lore, an external memory ledger via MCP. You MUST NOT rely on your internal context window for long-running tasks. You MUST follow this exact loop:

1. **FIRST CALL**: `switch_project(name, root_path)` to set context. Do this at the start of every session.
2. **COLD START**: Call `get_next_steps()` to get a briefing on pending work, blocked tasks, and recent lessons — no need to resume prior context.
3. **NEW TASKS**: When the user gives you a new goal, call `start_task(description)` BEFORE generating any code.
4. **SUBTASKS**: If a task involves 3+ distinct steps, decompose it — call `start_task(description, parent_task_id)` for each subtask.
5. **PROPOSING SOLUTIONS**: Before writing code, call `propose_attempt(task_id, approach)`.
6. **HANDLING FAILURES**: If the user reports an error, IMMEDIATELY call `log_outcome(attempt_id, 'rejected', reasoning, code_snippet)` BEFORE suggesting a fix. Include the actual code that failed.
7. **DO NOT AUTO-ACCEPT**: Only call `log_outcome(attempt_id, 'accepted', reasoning, code_snippet)` when the USER explicitly confirms success. If unsure, use `'pending'`. Include the actual code that was written.
8. **CONTEXT RECOVERY**: If you feel lost or the user says "try something else", call `review_ledger(task_id)` to read past failures so you don't repeat them.
9. **PERIODIC CHECK**: Call `get_active_context()` every ~5 messages to stay grounded.
10. **PROTOCOL REFRESH**: If unsure what to do next, call `get_protocol()` to re-read these rules.
11. **TASK COMPLETION**: After user confirms success, call `complete_task(task_id, lesson)` to extract a lesson. The resolved attempt is auto-detected from the last accepted attempt.
12. **CONTEXT PRESERVATION**: When context usage reaches 97%, IMMEDIATELY call `generate_handoff()` to preserve task state before context compression. Do NOT wait for the user to remind you.

Violation of these rules causes context rot and repeated failures.
