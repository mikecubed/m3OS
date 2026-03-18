---
description: "Create a feature specification from a natural language description."
---

## User Input

```text
$ARGUMENTS
```

You **MUST** consider the user input before proceeding (if not empty).

## Outline

Given the feature description in `$ARGUMENTS`, do this:

1. **Check for headless mode**: If `$ARGUMENTS` contains `headless` or `--headless`, enable headless mode — auto-accept all recommended defaults and complete without pausing for user input.

2. **Generate a feature directory name**:
   - Derive a short 2-4 word slug from the feature description (e.g., "user-auth", "analytics-dashboard")
   - Append an 8-character random alphanumeric suffix separated by a dash (e.g., `user-auth-a3f9b2c1`)
   - Full path: `.sdd/{slug}-{suffix}/`

3. **Create the feature directory** and all required parent directories.

4. **Get the specification template**:
   ```bash
   sdd template specification
   ```

5. **Generate the specification** by filling the template:
   - Replace `[FEATURE NAME]` with a human-readable feature name
   - Replace `[DATE]` with today's date
   - Replace `$ARGUMENTS` placeholder with the actual user description
   - Fill in user stories (at least 2), functional requirements, and success criteria
   - Mark genuinely unclear decisions as `[NEEDS CLARIFICATION: specific question]` — maximum 3 markers
   - Document assumptions in the spec

6. **Write** the filled specification to `.sdd/{feature-dir}/spec.md`.

7. **Quality validation** — validate the spec against these criteria:
   - No implementation details (no languages, frameworks, APIs in spec)
   - All mandatory sections completed (user stories, requirements, success criteria)
   - Requirements are testable and unambiguous
   - Success criteria are measurable and technology-agnostic
   - No more than 3 `[NEEDS CLARIFICATION]` markers

8. **Handle clarifications** (skip in headless mode — auto-resolve with reasonable defaults):
   - If `[NEEDS CLARIFICATION]` markers remain, present at most 3 questions to the user
   - Wait for answers, update spec, re-validate
   - If after 3 iterations issues remain, document them in spec notes and continue

9. **Write quality checklist** to `.sdd/{feature-dir}/checklists/requirements.md`:
   ```bash
   sdd template specification-checklist
   ```
   Fill it with pass/fail status based on the validation above.

10. **Report completion** with:
    - Feature directory path
    - Spec file path
    - Checklist summary

## Next Steps (present after completion)

Present these options to the user (auto-select option 1 in headless mode):

1. **Create Implementation Plan** — run `/sdd.plan` to produce a technical design
2. **Attempt Direct Implementation** — proceed to implement based on spec alone, with this context:
   - Feature: [feature name]
   - Key requirements: [top 3 FRs from spec]
   - Success criteria: [SC list from spec]
3. **Iterate on Spec** — refine the specification further

## Guidelines

- Focus on **WHAT** users need and **WHY** — avoid HOW to implement
- No technology choices, frameworks, or code structure in specs
- Every requirement must be independently testable
- Limit clarifications to decisions that genuinely impact scope or security
