---
description: "Create an implementation plan from a feature specification."
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
   - Otherwise, list `.sdd/` and ask the user which feature to plan (or auto-select the most recent in headless mode)
   - If no spec exists: offer to run `/sdd.specify` first, proceed ad-hoc, or accept a manual path

3. **Load the specification**: Read `.sdd/{feature-dir}/spec.md`

4. **Get the plan template**:
   ```bash
   sdd template plan
   ```

5. **Research phase** — for each NEEDS CLARIFICATION in the technical context:
   - Research the unknown using available context and best practices
   - Document decisions in a `research.md` file in the feature directory
   - Resolve ALL unknowns before proceeding

6. **Fill the plan template**:
   - Technical context (language, deps, storage, testing, platform, type, goals, constraints)
   - Project structure (source layout with real paths)
   - Research findings (key decisions and rationale)
   - Data model (entities, relationships, validation rules)
   - Interface contracts (what this feature exposes to users or other systems)

7. **Write** the plan to `.sdd/{feature-dir}/plan.md`

8. **Generate supporting artifacts** (as needed):
   - `data-model.md` — if the feature involves significant data entities
   - `contracts/` — if the feature exposes public interfaces (APIs, CLI schemas, etc.)
   - `research.md` — technical decisions and alternatives considered

9. **Report completion** with paths to all generated artifacts.

## Next Steps (present after completion)

1. **Generate Task List** — run `/sdd.tasks` to break the plan into actionable tasks
2. **Attempt Direct Implementation** — proceed now with this context:
   - Feature: [feature name]
   - Tech stack: [from plan technical context]
   - Key requirements: [top FRs from spec]
   - Success criteria: [SC list]
3. **Iterate on Plan** — refine the plan further
