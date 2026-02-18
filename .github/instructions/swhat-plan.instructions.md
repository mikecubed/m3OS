# sdd.plan — Create Implementation Plan

When the user asks to "plan a feature", "create an implementation plan", "map out how to build [feature]", or wants to move from specification to planning, follow this workflow.

## Trigger Phrases

- "plan [feature]"
- "create a plan for [feature]"
- "map out how to build [feature]"
- "implementation plan for [feature]"
- After completing a spec: user selects "Create implementation plan"

## Workflow

### Step 0: Locate Specification

Check conversation history and the `.sdd/` directory for a specification (spec.md).

**If no specification found**, ask the user:
1. **Generate a new spec** — Run the sdd.specify workflow first
2. **Plan without a spec** — Gather ad-hoc details via targeted questions about actors, actions, data, constraints, success criteria
3. **Provide location manually** — User points to an existing spec file

### Step 1: Setup

Run `sdd template plan` to retrieve the plan template. Create `plan.md` in the same directory as `spec.md`.

### Step 2: Load Context

Read the project to understand the codebase:
1. Read `spec.md` (the feature specification)
2. Read any instruction files: `AGENTS.md`, `COPILOT.md`, `README.md`, `.github/copilot-instructions.md`
3. Scan `docs/` folder contents
4. Generate a directory structure map
5. Use findings to inform Technical Context and Project Structure sections

### Step 3: Execute Plan Workflow

Fill the plan template:

**Technical Context:**
- Language/Version, Primary Dependencies, Storage, Testing, Target Platform
- Project Type, Performance Goals, Constraints, Scale/Scope
- Mark unknowns as "NEEDS CLARIFICATION"

**Constitution Check:**
- Gate: Must pass before Phase 0 research

**Phase 0 — Outline & Research:**
1. Extract unknowns from Technical Context → research tasks
2. Research each unknown, dependency, and integration
3. Consolidate findings in `research.md`: Decision, Rationale, Alternatives considered

**Phase 1 — Design & Contracts:**
1. Extract entities from spec → `data-model.md`: entity name, fields, relationships, validation rules
2. Generate API contracts from functional requirements → `contracts/` directory
3. Create `quickstart.md`

### Step 4: Report

If successful and ready for tasks:
- Output the **entire contents** of each created file verbatim: `plan.md`, `research.md`, `data-model.md`, `contracts/*`, `quickstart.md`
- Do NOT share temporary folder paths

If NOT ready (pending decisions, incomplete steps):
- Tell the user what the next step is
- List pending decisions needing input
- List incomplete phases/steps

### Step 5: Next Steps (only if plan is fully complete)

Present options to the user:
1. **Iterate on this plan** — Refine or clarify specific aspects
2. **Generate tasks** — Create a detailed task list (sdd.tasks workflow)
3. **Attempt to implement** — Start implementation with a general-purpose agent

## Key Rules

- Use absolute paths when writing files
- ERROR on gate failures or unresolved clarifications
- Re-check constitution after Phase 1 design
