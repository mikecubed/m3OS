# Feature Request Workflow

When the user asks to implement, build, create, or add a **new feature**, follow this workflow BEFORE writing any code. This ensures requirements are clear and saves hours of rework.

## When This Applies

- User asks to implement, build, create, or add a new feature
- Feature requests involving significant new functionality

## When This Does NOT Apply

- Bug fixes with clear reproduction steps
- Small tweaks to existing features ("change the button color")
- Refactoring with no behavior change
- Documentation updates
- User explicitly says "just do it" or "skip the spec"

## Workflow

### Step 0: Confirm Specification Attempt

Ask the user: "Would you like to proceed with a detailed specification attempt for this feature?"

- **If No**: Respond "Understood. Proceeding with original ask..." and implement directly
- **If Yes**: Continue to Step 1

### Step 1: Run Specification Workflow

Follow the sdd-specify instructions to create a feature specification:
1. Generate feature short name
2. Extract requirements from the user's description
3. Fill the spec template (run `sdd template specification`)
4. Validate quality (run `sdd template specification-checklist`)
5. Resolve any [NEEDS CLARIFICATION] items with the user
6. Output the complete spec

### Step 2: Offer Next Steps

After the spec is complete, present options:
1. **Iterate on this spec** — Refine specific aspects
2. **Create implementation plan** — Follow sdd-plan instructions
3. **Attempt to implement** — Start coding immediately

### Step 3: Planning (if selected)

Follow the sdd-plan instructions:
1. Load context from codebase and spec
2. Fill technical context, run research
3. Design data model and contracts
4. Output complete plan

### Step 4: Task Generation (if selected)

Follow the sdd-tasks instructions:
1. Load spec and plan
2. Generate dependency-ordered tasks by user story
3. Output task list with parallel opportunities

### Step 5: Implementation (if selected)

Launch implementation using a general-purpose agent with this context:
- Feature summary from spec
- Key requirements and success criteria
- Instructions to implement incrementally, test as you go, and ask about blockers

## Key Principle

Writing code for unclear requirements wastes time. A short conversation about requirements saves hours of rework. Build what the user actually needs, not what they initially said.
