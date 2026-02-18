---
description: Generate an actionable, dependency-ordered tasks.md for the feature based on available design artifacts.
---

## User Input

```text
$ARGUMENTS
```

You **MUST** consider the user input before proceeding (if not empty).

## Headless Mode

If the user input contains "headless" or "--headless", operate in **headless mode**:
- Automatically accept all recommended options for clarifications
- Make informed decisions using defaults and best practices instead of asking
- Do not pause for user confirmation at decision points
- Continue through the entire workflow without interruption
- Still output the final artifacts and summary

## Outline

0. **Prerequisite - Locate Artifacts**: Check if the conversation history references a specification (spec.md) and plan (plan.md).

   **If no specification is found**, present the user with options:

   > "There is no specification that I can find right now. Would you like to:
   > 1. **Generate a new spec** - Create a specification for this feature
   > 2. **Provide location manually** - Point me to an existing specification"

   **Handle user selection**:

   - **If Option 1 (Generate a new spec)**:
     1. Ask the user: "Please describe the feature you want to implement."
     2. Execute the `/sdd.specify` command flow with the user's description
     3. After specification is complete, prompt user to run `/sdd.plan`
     4. After plan is complete, continue to step 1

   - **If Option 2 (Provide location manually)**:
     1. Parse the user's response for file path or location details
     2. If path provided, attempt to read the specification file
     3. If path not found or invalid, work with user to locate spec.md
     4. Once spec is loaded, check for plan.md (see below)

   **If specification is found but no plan**, present the user with options:

   > "I found the specification, but there is no plan. Would you like to:
   > 1. **Generate a plan** - Create an implementation plan from the spec
   > 2. **Provide location manually** - Point me to an existing plan"

   **Handle user selection**:

   - **If Option 1 (Generate a plan)**:
     1. Execute the `/sdd.plan` command flow
     2. After plan is complete, continue to step 1

   - **If Option 2 (Provide location manually)**:
     1. Parse the user's response for file path or location details
     2. If path provided, attempt to read the plan file
     3. If path not found or invalid, work with user to locate plan.md
     4. Once plan is loaded, continue to step 1

   **If both specification and plan are found**, proceed to step 1.

1. **Setup**:
   a. Run `sdd template tasks` to retrieve the tasks template
   b. Create `tasks.md` in the same directory where `plan.md` is located
   c. Write the template content to `tasks.md` as the starting point

2. **Load context**: Read the available design artifacts:
   a. Read `plan.md` (tech stack, libraries, project structure)
   b. Read `spec.md` (user stories with priorities P1, P2, P3, etc.)
   c. Note: Generate tasks based on what's available

3. **Execute task generation workflow**:
   - Extract tech stack, libraries, and project structure from plan.md
   - Extract user stories with their priorities from spec.md
   - Generate tasks organized by user story (see Task Generation Rules below)
   - Generate dependency graph showing user story completion order
   - Create parallel execution examples per user story
   - Validate task completeness (each user story has all needed tasks, independently testable)

4. **Generate tasks.md**: Fill the template with:
   - Correct feature name from plan.md
   - Phase 1: Setup tasks (project initialization)
   - Phase 2: Foundational tasks (blocking prerequisites for all user stories)
   - Phase 3+: One phase per user story (in priority order from spec.md)
   - Each phase includes: story goal, independent test criteria, implementation tasks
   - Final Phase: Polish & cross-cutting concerns
   - All tasks must follow the strict checklist format (see Task Generation Rules)
   - Clear file paths for each task
   - Dependencies section showing story completion order
   - Parallel execution examples per story
   - Implementation strategy section (MVP first, incremental delivery)

5. **Report**: Output the generated tasks.md and summary:
   - **CRITICAL: Output the ENTIRE contents of tasks.md verbatim** - do not summarize or paraphrase
   - Total task count
   - Task count per user story
   - Parallel opportunities identified
   - Independent test criteria for each story
   - Suggested MVP scope (typically just User Story 1)
   - Format validation: Confirm ALL tasks follow the checklist format

Context for task generation: $ARGUMENTS

The tasks.md should be immediately executable - each task must be specific enough that an LLM can complete it without additional context.

## Task Generation Rules

**CRITICAL**: Tasks MUST be organized by user story to enable independent implementation and testing.

**Tests are OPTIONAL**: Only generate test tasks if explicitly requested in the feature specification or if user requests TDD approach.

### Checklist Format (REQUIRED)

Every task MUST strictly follow this format:

```text
- [ ] [TaskID] [P?] [Story?] Description with file path
```

**Format Components**:

1. **Checkbox**: ALWAYS start with `- [ ]` (markdown checkbox)
2. **Task ID**: Sequential number (T001, T002, T003...) in execution order
3. **[P] marker**: Include ONLY if task is parallelizable (different files, no dependencies on incomplete tasks)
4. **[Story] label**: REQUIRED for user story phase tasks only
   - Format: [US1], [US2], [US3], etc. (maps to user stories from spec.md)
   - Setup phase: NO story label
   - Foundational phase: NO story label
   - User Story phases: MUST have story label
   - Polish phase: NO story label
5. **Description**: Clear action with exact file path

**Examples**:

- CORRECT: `- [ ] T001 Create project structure per implementation plan`
- CORRECT: `- [ ] T005 [P] Implement authentication middleware in src/middleware/auth.py`
- CORRECT: `- [ ] T012 [P] [US1] Create User model in src/models/user.py`
- CORRECT: `- [ ] T014 [US1] Implement UserService in src/services/user_service.py`
- WRONG: `- [ ] Create User model` (missing ID and file path)
- WRONG: `T001 [US1] Create model` (missing checkbox)
- WRONG: `- [ ] [US1] Create User model` (missing Task ID)
- WRONG: `- [ ] T001 [US1] Create model` (missing file path)

### Task Organization

1. **From User Stories (spec.md)** - PRIMARY ORGANIZATION:
   - Each user story (P1, P2, P3...) gets its own phase
   - Map all related components to their story:
     - Models needed for that story
     - Services needed for that story
     - Endpoints/UI needed for that story
     - If tests requested: Tests specific to that story
   - Mark story dependencies (most stories should be independent)

2. **From Data Model**:
   - Map each entity to the user story(ies) that need it
   - If entity serves multiple stories: Put in earliest story or Setup phase
   - Relationships -> service layer tasks in appropriate story phase

3. **From Setup/Infrastructure**:
   - Shared infrastructure -> Setup phase (Phase 1)
   - Foundational/blocking tasks -> Foundational phase (Phase 2)
   - Story-specific setup -> within that story's phase

### Phase Structure

- **Phase 1**: Setup (project initialization)
- **Phase 2**: Foundational (blocking prerequisites - MUST complete before user stories)
- **Phase 3+**: User Stories in priority order (P1, P2, P3...)
  - Within each story: Tests (if requested) -> Models -> Services -> Endpoints -> Integration
  - Each phase should be a complete, independently testable increment
- **Final Phase**: Polish & Cross-Cutting Concerns

## Key Rules

- Use absolute paths when writing files
- Each task must be specific enough for an LLM to execute without additional context
- Organize by user story to enable independent, incremental delivery
- Mark parallelizable tasks with [P] to maximize execution efficiency
