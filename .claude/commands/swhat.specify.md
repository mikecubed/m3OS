---
description: Create or update the feature specification from a natural language feature description.
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

The text the user typed after `/sdd.specify` in the triggering message **is** the feature description. The `$ARGUMENTS` placeholder above contains that text. Do not ask the user to repeat it unless they provided an empty command.

Given that feature description, do this:

1. **Generate a concise short name** (2-4 words) for the feature directory:
   - Analyze the feature description and extract the most meaningful keywords
   - Create a 2-4 word short name that captures the essence of the feature
   - Use action-noun format when possible (e.g., "add-user-auth", "fix-payment-bug")
   - Preserve technical terms and acronyms (OAuth2, API, JWT, etc.)
   - Keep it concise but descriptive enough to understand the feature at a glance
   - **Append 12 random alphanumeric characters** to the short name, separated by an underscore
   - Generate the random characters using lowercase letters and digits (a-z, 0-9)
   - Examples:
     - "I want to add user authentication" -> "user-auth_a3b7x9k2m4n1"
     - "Implement OAuth2 integration for the API" -> "oauth2-api-integration_p8q2w5e1r7t3"
     - "Create a dashboard for analytics" -> "analytics-dashboard_j6h4f2d9s1l8"
     - "Fix payment processing timeout bug" -> "fix-payment-timeout_c5v3b7n9m2k4"

2. Retrieve the specification template by running `sdd template specification` to understand required sections.

3. Follow this execution flow:

    1. Parse user description from Input
       If empty: ERROR "No feature description provided"
    2. Extract key concepts from description
       Identify: actors, actions, data, constraints
    3. For unclear aspects:
       - Make informed guesses based on context and industry standards
       - Only mark with [NEEDS CLARIFICATION: specific question] if:
         - The choice significantly impacts feature scope or user experience
         - Multiple reasonable interpretations exist with different implications
         - No reasonable default exists
       - **LIMIT: Maximum 3 [NEEDS CLARIFICATION] markers total**
       - Prioritize clarifications by impact: scope > security/privacy > user experience > technical details
    4. Fill User Scenarios & Testing section
       If no clear user flow: ERROR "Cannot determine user scenarios"
    5. Generate Functional Requirements
       Each requirement must be testable
       Use reasonable defaults for unspecified details (document assumptions in Assumptions section)
    6. Define Success Criteria
       Create measurable, technology-agnostic outcomes
       Include both quantitative metrics (time, performance, volume) and qualitative measures (user satisfaction, task completion)
       Each criterion must be verifiable without implementation details
    7. Identify Key Entities (if data involved)
    8. Return: SUCCESS (spec ready for planning)

4. Write the specification to `.sdd/{FEATURE_SHORT_NAME}/{SPEC_FILE}` using the template structure, replacing placeholders with concrete details derived from the feature description (arguments) while preserving section order and headings.

5. **Specification Quality Validation**: After writing the initial spec, validate it against quality criteria:

   a. **Create Spec Quality Checklist**: Retrieve the checklist template by running `sdd template specification-checklist` and write it to `.sdd/{FEATURE_SHORT_NAME}/requirements.md`, replacing placeholders with feature-specific values.

   b. **Run Validation Check**: Review the spec against each checklist item:
      - For each item, determine if it passes or fails
      - Document specific issues found (quote relevant spec sections)

   c. **Handle Validation Results**:

      - **If all items pass**: Mark checklist complete and proceed to step 5

      - **If items fail (excluding [NEEDS CLARIFICATION])**:
        1. List the failing items and specific issues
        2. Update the spec to address each issue
        3. Re-run validation until all items pass (max 3 iterations)
        4. If still failing after 3 iterations, document remaining issues in checklist notes and warn user

      - **If [NEEDS CLARIFICATION] markers remain**:
        1. Extract all [NEEDS CLARIFICATION: ...] markers from the spec
        2. **LIMIT CHECK**: If more than 3 markers exist, keep only the 3 most critical (by scope/security/UX impact) and make informed guesses for the rest
        3. For each clarification needed (max 3), present options to user in this format:

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

        4. **CRITICAL - Table Formatting**: Ensure markdown tables are properly formatted:
           - Use consistent spacing with pipes aligned
           - Each cell should have spaces around content: `| Content |` not `|Content|`
           - Header separator must have at least 3 dashes: `|--------|`
           - Test that the table renders correctly in markdown preview
        5. Number questions sequentially (Q1, Q2, Q3 - max 3 total)
        6. Present all questions together before waiting for responses
        7. Wait for user to respond with their choices for all questions (e.g., "Q1: A, Q2: Custom - [details], Q3: B")
        8. Update the spec by replacing each [NEEDS CLARIFICATION] marker with the user's selected or provided answer
        9. Re-run validation after all clarifications are resolved

   d. **Update Checklist**: After each validation iteration, update the checklist file with current pass/fail status

6. **Report**:
   - **CRITICAL: Output the ENTIRE contents of spec.md verbatim** - do not summarize, paraphrase, or create tables. Show the full markdown file.
   - **DO NOT** share the location of any temporary folders in the final response.
   - After the spec content, state whether the specification was **successful** (all checklist items pass, no ambiguities) or **needs refinement** (details are still too vague, clarifications needed)
   - If needs refinement: Explain what aspects are unclear and suggest the user provide more details

7. **Next Steps** (only if specification was successful):
   - After outputting the spec, present the user with next step options:
     - **Option 1: "Iterate on this plan"** - Refine or clarify specific aspects of the specification
     - **Option 2: "Help me map out how to accomplish this"** - Create a detailed implementation plan
     - **Option 3: "Attempt to implement"** - Start implementation now, iterating as needed

   - **Handle user selection**:

     - **If Option 1 (Iterate)**:
       1. Ask the user: "What aspects of the specification would you like to refine or clarify?"
       2. Wait for user response
       3. Update the spec.md based on their feedback
       4. Re-run validation and output the updated spec
       5. Return to the Next Steps prompt

     - **If Option 2 (Map out implementation)**:
       1. Execute the `/sdd.plan` command to create a detailed implementation plan
       2. The spec.md is already in conversation history, so no additional context is needed

     - **If Option 3 (Attempt to implement)**:
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

## General Guidelines

## Quick Guidelines

- Focus on **WHAT** users need and **WHY**.
- Avoid HOW to implement (no tech stack, APIs, code structure).
- Written for business stakeholders, not developers.
- DO NOT create any checklists that are embedded in the spec. That will be a separate command.

### Section Requirements

- **Mandatory sections**: Must be completed for every feature
- **Optional sections**: Include only when relevant to the feature
- When a section doesn't apply, remove it entirely (don't leave as "N/A")

### For AI Generation

When creating this spec from a user prompt:

1. **Make informed guesses**: Use context, industry standards, and common patterns to fill gaps
2. **Document assumptions**: Record reasonable defaults in the Assumptions section
3. **Limit clarifications**: Maximum 3 [NEEDS CLARIFICATION] markers - use only for critical decisions that:
   - Significantly impact feature scope or user experience
   - Have multiple reasonable interpretations with different implications
   - Lack any reasonable default
4. **Prioritize clarifications**: scope > security/privacy > user experience > technical details
5. **Think like a tester**: Every vague requirement should fail the "testable and unambiguous" checklist item
6. **Common areas needing clarification** (only if no reasonable default exists):
   - Feature scope and boundaries (include/exclude specific use cases)
   - User types and permissions (if multiple conflicting interpretations possible)
   - Security/compliance requirements (when legally/financially significant)

**Examples of reasonable defaults** (don't ask about these):

- Data retention: Industry-standard practices for the domain
- Performance targets: Standard web/mobile app expectations unless specified
- Error handling: User-friendly messages with appropriate fallbacks
- Authentication method: Standard session-based or OAuth2 for web apps
- Integration patterns: RESTful APIs unless specified otherwise

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

- "API response time is under 200ms" (too technical, use "Users see results instantly")
- "Database can handle 1000 TPS" (implementation detail, use user-facing metric)
- "React components render efficiently" (framework-specific)
- "Redis cache hit rate above 80%" (technology-specific)
