---
description: "Generate an actionable, dependency-ordered task list from the feature plan."
---

## User Input

```text
$ARGUMENTS
```

You **MUST** consider the user input before proceeding (if not empty).

## Outline

1. **Check for headless mode**: If `$ARGUMENTS` contains `headless` or `--headless`, enable headless mode.

2. **Locate the feature directory**:
   - If a path or feature name is provided in `$ARGUMENTS`, use it
   - Otherwise, list `.sdd/` and ask the user (or auto-select most recent in headless mode)
   - If no plan exists: offer to run `/sdd.plan` first, proceed without it, or accept a manual path

3. **Load design documents** from `.sdd/{feature-dir}/`:
   - **Required**: `plan.md`, `spec.md`
   - **Optional**: `data-model.md`, `contracts/`, `research.md`

4. **Get the tasks template**:
   ```bash
   sdd template tasks
   ```

5. **Generate the task list**:
   - Extract user stories from spec.md (with their priorities P1, P2, P3...)
   - Extract tech stack and project structure from plan.md
   - Organize tasks by user story — each story gets its own phase
   - Every task must follow the strict format: `- [ ] T### [P?] [US?] Description with file path`
   - Mark parallelizable tasks with `[P]`
   - Include dependency graph and parallel execution examples

6. **Write** tasks to `.sdd/{feature-dir}/tasks.md`

7. **Report**:
   - Total task count
   - Task count per user story
   - Parallel opportunities
   - Suggested MVP scope (typically User Story 1 only)

## Task Format

Every task MUST follow this exact format:
```
- [ ] T### [P?] [US?] Description with exact/file/path.ext
```

Examples:
- `- [ ] T001 Create project structure per implementation plan`
- `- [ ] T005 [P] Implement auth middleware in src/middleware/auth.py`
- `- [ ] T012 [P] [US1] Create User model in src/models/user.py`

## Next Steps (present after completion)

1. **Implement** — begin working through tasks.md in order
2. **Attempt Direct Implementation** — skip tasks and implement with this context:
   - Feature: [feature name]
   - Tech stack: [from plan]
   - Key requirements: [top FRs from spec]
   - Success criteria: [SC list from spec]
3. **Iterate on Tasks** — refine the task list
