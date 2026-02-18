---
name: sdd-feature-workflow
description: When the user asks to implement, build, create, or add a new feature, use this workflow to clarify requirements and create a specification before writing code. Activates for feature requests, not bug fixes or small tweaks.
user-invocable: false
---

# Feature Request Workflow

When the user asks you to implement, build, create, or add a **new feature**, follow this complete workflow BEFORE writing any code.

## When This Skill Does NOT Apply

- Bug fixes with clear reproduction steps
- Small tweaks to existing features ("change the button color")
- Refactoring with no behavior change
- Documentation updates
- User explicitly says "just do it" or "skip the spec"

---

## Step 0: Confirm Specification Attempt

Before proceeding, ask the user:

> "Would you like to proceed with a detailed specification attempt for this feature?"

**If user chooses No**:
- Do NOT proceed with the specification workflow
- Respond: "Understood. Proceeding with original ask..."
- Proceed with the user's original request without the specification workflow

**If user chooses Yes**:
- Proceed to Step 1

---

## Step 1: Generate Feature Short Name

Create a concise short name (2-4 words) for the feature:

- Analyze the feature description and extract the most meaningful keywords
- Use action-noun format when possible (e.g., "add-user-auth", "fix-payment-bug")
- Preserve technical terms and acronyms (OAuth2, API, JWT, etc.)
- Keep it concise but descriptive enough to understand the feature at a glance
- **Append 12 random alphanumeric characters** to the short name, separated by an underscore
- Generate the random characters using lowercase letters and digits (a-z, 0-9)

**Examples**:
- "I want to add user authentication" -> "user-auth_a3b7x9k2m4n1"
- "Implement OAuth2 integration for the API" -> "oauth2-api-integration_p8q2w5e1r7t3"
- "Create a dashboard for analytics" -> "analytics-dashboard_j6h4f2d9s1l8"

---

## Step 2: Analyze and Extract Requirements

Parse the user's feature description and extract key concepts:

1. **Identify actors**: Who uses this feature? (user roles, personas)
2. **Identify actions**: What can they do? (core behaviors)
3. **Identify data**: What information is involved? (entities, attributes)
4. **Identify constraints**: What limits or rules apply? (validation, permissions)

### Handling Unclear Aspects

- **Make informed guesses** based on context and industry standards
- Only mark with `[NEEDS CLARIFICATION: specific question]` if:
  - The choice significantly impacts feature scope or user experience
  - Multiple reasonable interpretations exist with different implications
  - No reasonable default exists
- **LIMIT: Maximum 3 [NEEDS CLARIFICATION] markers total**
- **Prioritize clarifications by impact**: scope > security/privacy > user experience > technical details

---

## Step 3: Retrieve and Fill Specification Template

Run this command to get the template:

```bash
sdd template specification
```

Write the specification to `.sdd/{FEATURE_SHORT_NAME}/spec.md` using the template structure.

### Execution Flow

1. If description is empty: ERROR "No feature description provided"
2. Extract key concepts (actors, actions, data, constraints)
3. Fill **User Scenarios & Testing** section
   - If no clear user flow: ERROR "Cannot determine user scenarios"
   - Each user story must be independently testable
   - Include Given/When/Then acceptance scenarios
4. Generate **Functional Requirements**
   - Each requirement must be testable
   - Use reasonable defaults for unspecified details
5. Define **Success Criteria**
   - Create measurable, technology-agnostic outcomes
   - Include quantitative metrics (time, performance, volume)
   - Include qualitative measures (user satisfaction, task completion)
   - Each criterion must be verifiable without implementation details
6. Identify **Key Entities** (if data involved)

---

## Step 4: Validate Specification Quality

Run this command to get the checklist:

```bash
sdd template specification-checklist
```

Write the checklist to `.sdd/{FEATURE_SHORT_NAME}/requirements.md` and validate:

### Validation Check

For each checklist item:
- Determine if it passes or fails
- Document specific issues found (quote relevant spec sections)

### Handle Validation Results

**If all items pass**: Mark checklist complete and proceed.

**If items fail (excluding [NEEDS CLARIFICATION])**:
1. List the failing items and specific issues
2. Update the spec to address each issue
3. Re-run validation until all items pass (max 3 iterations)
4. If still failing after 3 iterations, document remaining issues and warn user

**If [NEEDS CLARIFICATION] markers remain**:
1. Extract all `[NEEDS CLARIFICATION: ...]` markers from the spec
2. **LIMIT CHECK**: If more than 3 markers exist, keep only the 3 most critical and make informed guesses for the rest
3. Present options to user in this format:

```markdown
## Question [N]: [Topic]

**Context**: [Quote relevant spec section]

**What we need to know**: [Specific question from NEEDS CLARIFICATION marker]

**Suggested Answers**:

| Option | Answer | Implications |
|--------|--------|--------------|
| A      | [First suggested answer] | [What this means for the feature] |
| B      | [Second suggested answer] | [What this means for the feature] |
| C      | [Third suggested answer] | [What this means for the feature] |
| Custom | Provide your own answer | [Explain how to provide custom input] |

**Your choice**: _[Wait for user response]_
```

4. Number questions sequentially (Q1, Q2, Q3 - max 3 total)
5. Present all questions together before waiting for responses
6. After user responds, update spec with their answers
7. Re-run validation after all clarifications are resolved

---

## Step 5: Report

1. **CRITICAL: Output the ENTIRE contents of spec.md verbatim** - do not summarize, paraphrase, or create tables. Show the full markdown file.
2. **DO NOT** share the location of any temporary folders in the final response.
3. After the spec content, report status:
   - **Successful**: All checklist items pass, no ambiguities - spec is ready for implementation
   - **Needs refinement**: Details are still too vague - explain what aspects are unclear

4. **If needs refinement**, explain what aspects are unclear and suggest the user provide more details, then proceed to Step 6.

5. **If successful**, proceed to Step 6.

---

## Step 6: Next Steps

After outputting the spec, present the user with next step options:

- **Option 1: "Iterate on this plan"** - Refine or clarify specific aspects of the specification
- **Option 2: "Help me map out how to accomplish this"** - Create a detailed implementation plan
- **Option 3: "Attempt to implement"** - Start implementation now, iterating as needed

### Handle User Selection

**If Option 1 (Iterate on this plan)**:
1. Ask the user: "What aspects of the specification would you like to refine or clarify?"
2. Wait for user response
3. Update the spec.md based on their feedback
4. Re-run validation and output the updated spec
5. Return to the Next Steps prompt

**If Option 2 (Help me map out how to accomplish this)**:
1. Execute the `/sdd.plan` command to create a detailed implementation plan
2. The spec.md is already in conversation history, so no additional context is needed

**If Option 3 (Attempt to implement)**:
1. Create a new task/agent to handle implementation
2. Provide the agent with this context:

```
You are implementing a feature based on the following specification.

## Feature Summary
[Summarize the key points from spec.md: feature name, main user stories, core functional requirements, and success criteria]

## Your Instructions
1. Analyze the specification and the current codebase
2. Determine the best approach to implement this feature
3. If you need clarification on HOW to accomplish any requirement, ask the user
4. Implement the feature incrementally, testing as you go
5. If you encounter blockers or need decisions, ask the user before proceeding
6. Focus on delivering a working MVP that satisfies the P1 user story first

## Key Requirements
[List the functional requirements from the spec]

## Success Criteria
[List the measurable outcomes from the spec]

Begin by exploring the codebase and proposing your implementation approach.
```

3. Let the implementation agent take over

---

## Guidelines

### Focus on WHAT and WHY, Not HOW

- Focus on **WHAT** users need and **WHY**
- Avoid HOW to implement (no tech stack, APIs, code structure)
- Written for business stakeholders, not developers
- DO NOT create any checklists embedded in the spec

### Section Requirements

- **Mandatory sections**: Must be completed for every feature
- **Optional sections**: Include only when relevant to the feature
- When a section doesn't apply, remove it entirely (don't leave as "N/A")

### Making Informed Guesses

When creating specs, use reasonable defaults instead of asking about:

- **Data retention**: Industry-standard practices for the domain
- **Performance targets**: Standard web/mobile app expectations unless specified
- **Error handling**: User-friendly messages with appropriate fallbacks
- **Authentication method**: Standard session-based or OAuth2 for web apps
- **Integration patterns**: RESTful APIs unless specified otherwise

### Success Criteria Guidelines

Success criteria must be:

1. **Measurable**: Include specific metrics (time, percentage, count, rate)
2. **Technology-agnostic**: No mention of frameworks, languages, databases, or tools
3. **User-focused**: Describe outcomes from user/business perspective, not system internals
4. **Verifiable**: Can be tested/validated without knowing implementation details

**Good examples**:
- "Users can complete checkout in under 3 minutes"
- "System supports 10,000 concurrent users"
- "95% of searches return results in under 1 second"
- "Task completion rate improves by 40%"

**Bad examples** (implementation-focused):
- "API response time is under 200ms" (too technical)
- "Database can handle 1000 TPS" (implementation detail)
- "React components render efficiently" (framework-specific)
- "Redis cache hit rate above 80%" (technology-specific)

---

## Why This Matters

Writing code for unclear requirements wastes time. A short conversation about requirements saves hours of rework. Your job is to build what the user actually needs, not what they initially said.
