# sdd.specify — Create Feature Specification

When the user asks to "specify a feature", "create a spec", "write a specification", or uses language like "spec out [feature]", follow this workflow.

## Trigger Phrases

- "specify [feature]"
- "create a spec for [feature]"
- "spec out [feature]"
- "write a specification for [feature]"

## Workflow

### Step 1: Validate Input

The user's message contains the feature description. If it's empty or too vague to extract actors/actions/data, ask the user to describe the feature they want to implement.

### Step 2: Generate Feature Short Name

Create a concise 2-4 word short name for the feature directory:
- Use action-noun format (e.g., "user-auth", "payment-flow")
- Preserve technical terms and acronyms
- Append 12 random alphanumeric characters separated by underscore
- Examples: "user-auth_a3b7x9k2m4n1", "analytics-dashboard_j6h4f2d9s1l8"

### Step 3: Retrieve Template

Run `sdd template specification` to get the spec template structure.

### Step 4: Extract Requirements

Parse the feature description and extract:
1. **Actors**: Who uses this feature?
2. **Actions**: What can they do?
3. **Data**: What information is involved?
4. **Constraints**: What limits or rules apply?

**Handling unclear aspects:**
- Make informed guesses based on context and industry standards
- Only use `[NEEDS CLARIFICATION: specific question]` if the choice significantly impacts scope, has multiple reasonable interpretations, or lacks any reasonable default
- **Maximum 3 [NEEDS CLARIFICATION] markers total**
- Priority: scope > security/privacy > user experience > technical details

**Reasonable defaults (don't ask about these):**
- Data retention: Industry-standard practices
- Performance targets: Standard expectations unless specified
- Error handling: User-friendly messages with fallbacks
- Auth: Standard session-based or OAuth2
- Integration: RESTful APIs unless specified

### Step 5: Fill Template and Write Spec

Write the specification to `.sdd/{FEATURE_SHORT_NAME}/spec.md` using the template structure. Fill all sections:

- **User Scenarios & Testing** (mandatory): Prioritized user stories (P1, P2, P3...), each independently testable with Given/When/Then acceptance scenarios
- **Requirements** (mandatory): Testable functional requirements (FR-001, FR-002...) and key entities if data is involved
- **Success Criteria** (mandatory): Measurable, technology-agnostic outcomes (SC-001, SC-002...)

**Success criteria must be:**
1. Measurable (time, percentage, count, rate)
2. Technology-agnostic (no frameworks, languages, databases)
3. User-focused (outcomes from user/business perspective)
4. Verifiable (testable without implementation details)

### Step 6: Validate Specification Quality

Run `sdd template specification-checklist` and write to `.sdd/{FEATURE_SHORT_NAME}/requirements.md`.

Validate the spec against the checklist:
- **All pass**: Proceed to report
- **Items fail**: Fix the spec, re-validate (max 3 iterations)
- **[NEEDS CLARIFICATION] remains**: Present each (max 3) to the user as a structured question with suggested options using the `ask_user` tool, then update the spec with answers

### Step 7: Report

Output the **entire contents** of spec.md verbatim. Do not summarize or paraphrase. State whether the spec is **successful** (ready for planning) or **needs refinement**.

### Step 8: Next Steps

Present options to the user:
1. **Iterate on this spec** — Refine or clarify specific aspects
2. **Create implementation plan** — Follow the sdd.plan workflow
3. **Attempt to implement** — Start coding with the spec as guide

## Guidelines

- Focus on **WHAT** users need and **WHY**, not HOW to implement
- Written for business stakeholders, not developers
- Do NOT embed checklists in the spec itself
- Mandatory sections must be completed; optional sections should be removed if not applicable
