# sdd.tasks — Generate Implementation Tasks

When the user asks to "generate tasks", "create a task list", "break down the plan into tasks", or wants to move from planning to task generation, follow this workflow.

## Trigger Phrases

- "generate tasks for [feature]"
- "create task list"
- "break this plan into tasks"
- "what tasks do I need?"
- After completing a plan: user selects "Generate tasks"

## Workflow

### Step 0: Locate Artifacts

Check conversation history and `.sdd/` directory for `spec.md` and `plan.md`.

**If no specification found**, ask the user:
1. **Generate a new spec** — Run the sdd.specify workflow first
2. **Provide location manually** — Point to an existing spec file

**If spec found but no plan**, ask the user:
1. **Generate a plan** — Run the sdd.plan workflow first
2. **Provide location manually** — Point to an existing plan file

### Step 1: Setup

Run `sdd template tasks` to retrieve the tasks template. Create `tasks.md` in the same directory as `plan.md`.

### Step 2: Load Context

Read available design artifacts:
- `plan.md`: tech stack, libraries, project structure
- `spec.md`: user stories with priorities (P1, P2, P3...)

### Step 3: Generate Tasks

Extract from plan and spec, then generate tasks organized by user story:

**Task Checklist Format (REQUIRED):**
```
- [ ] [TaskID] [P?] [Story?] Description with file path
```

Components:
- **Checkbox**: Always `- [ ]`
- **Task ID**: Sequential (T001, T002, T003...)
- **[P]**: Only if parallelizable (different files, no dependencies)
- **[Story]**: Required for user story phases only ([US1], [US2], etc.)
- **Description**: Clear action with exact file path

**Phase Structure:**
- **Phase 1**: Setup (project initialization) — no story labels
- **Phase 2**: Foundational (blocking prerequisites) — no story labels, MUST complete before user stories
- **Phase 3+**: One phase per user story in priority order — MUST have story labels
- **Final Phase**: Polish & cross-cutting concerns — no story labels

**Tests are OPTIONAL** — only include if explicitly requested in the spec.

### Step 4: Generate Dependencies

- Phase dependencies (Setup → Foundational → User Stories → Polish)
- User story dependencies (most should be independent)
- Within-story ordering: Models → Services → Endpoints → Integration
- Parallel execution examples per story

### Step 5: Report

Output the **entire contents** of `tasks.md` verbatim, plus:
- Total task count
- Task count per user story
- Parallel opportunities identified
- Suggested MVP scope (typically User Story 1 only)
- Format validation: confirm all tasks follow checklist format

### Step 6: Implementation Strategy

Include in the output:
- **MVP First**: Setup → Foundational → US1 → Validate → Deploy
- **Incremental Delivery**: Each story adds value independently
- **Parallel Strategy**: Multiple stories can proceed simultaneously after Foundational phase

## Key Rules

- Use absolute paths when writing files
- Each task must be specific enough for an AI agent to execute without additional context
- Organize by user story for independent, incremental delivery
- Mark parallelizable tasks with [P] for execution efficiency
- Every task needs an exact file path in its description
