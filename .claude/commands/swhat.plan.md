---
description: Execute the implementation planning workflow using the plan template to generate design artifacts.
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

0. **Prerequisite - Locate Specification**: Check if the conversation history references a specification (spec.md or equivalent).

   **If no specification is found**, present the user with options:

   > "There is no specification that I can find right now. Would you like to:
   > 1. **Generate a new spec** - Create a specification for this problem
   > 2. **Plan without a spec** - Attempt planning with ad-hoc details
   > 3. **Provide location manually** - Point me to an existing specification"

   **Handle user selection**:

   - **If Option 1 (Generate a new spec)**:
     1. Ask the user: "Please describe the feature you want to implement."
     2. Execute the `/sdd.specify` command flow with the user's description
     3. After specification is complete, continue to step 1

   - **If Option 2 (Plan without a spec)**:
     1. Dispatch a task to gather enough detail from the user to fill in the plan template
     2. Ask targeted questions about: actors, actions, data, constraints, success criteria
     3. Document responses as ad-hoc requirements in the plan's Summary section
     4. Mark the plan as "Ad-hoc (no formal spec)" and continue to step 1

   - **If Option 3 (Provide location manually)**:
     1. Parse the user's response for file path or location details
     2. If path provided, attempt to read the specification file
     3. If path not found or invalid, work with user to locate spec.md
     4. Once spec is loaded, continue to step 1

   **If specification is found**, proceed to step 1.

1. **Setup**:
   a. Run `sdd template plan` to retrieve the plan template
   b. Create `plan.md` in the same directory where `spec.md` is located
   c. Write the template content to `plan.md` as the starting point

2. **Load context**: Read the project directory to understand the codebase:
   a. Read `spec.md` (the feature specification from step 0)
   b. Look for and read any of these files if they exist:
      - `AGENTS.md` or `AGENTS.MD`
      - `AGENT_INSTRUCTIONS.md` or `AGENT_INSTRUCTIONS.MD`
      - `README.md`, `README`, or `README.*` (any extension)
   c. Check for `docs/` or `documentation/` folders and scan their contents
   d. Generate a directory structure map using `ls -la` or `tree` CLI commands
   e. Use findings to inform Technical Context and Project Structure sections of the plan

3. **Execute plan workflow**: Follow the structure in the plan template to:
   - Fill Technical Context (mark unknowns as "NEEDS CLARIFICATION")
   - Evaluate gates (ERROR if violations unjustified)
   - Phase 0: Generate research.md (resolve all NEEDS CLARIFICATION)
   - Phase 1: Generate data-model.md, contracts/, quickstart.md
   - Re-evaluate context agreement with plan

4. **Stop and report**: Command ends after Phase 1 planning.

   **If successful and ready to create tasks**:
   - **IMPORTANT**: DO NOT share tmp folder paths in the output
   - Output the **entire contents** of each file that was created:
     - `plan.md`
     - `research.md` (if created)
     - `data-model.md` (if created)
     - `contracts/*` files (if created)
     - `quickstart.md` (if created)
   - Proceed to step 5 (Next Steps)

   **If NOT ready** (pending decisions, incomplete steps, unresolved clarifications):
   - Tell the user what the next step is to finish this planning session
   - List any pending decisions that need user input
   - List any phases or steps not yet complete
   - Do NOT proceed to step 5 until planning is fully resolved

5. **Next Steps** (only if plan is fully successful, all phases complete, and no iteration required): After outputting the plan, present the user with options:

   - **Option 1: "Iterate on this plan"** - Refine or clarify specific aspects of the plan
   - **Option 2: "Generate tasks"** - Create a detailed, actionable task list for implementation
   - **Option 3: "Attempt to implement"** - Start implementation now, iterating as needed

   **Handle user selection**:

   - **If Option 1 (Iterate on this plan)**:
     1. Ask the user: "What aspects of the plan would you like to refine or clarify?"
     2. Wait for user response
     3. Update the plan.md based on their feedback
     4. Re-run validation and output the updated plan
     5. Return to the Next Steps prompt

   - **If Option 2 (Help me map out how to accomplish this)**:
     1. Execute the `/sdd.tasks` command to generate a detailed task list
     2. The plan.md and spec.md are already in conversation history, so no additional context is needed

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

## Phases

### Phase 0: Outline & Research

1. **Extract unknowns from Technical Context** above:
   - For each NEEDS CLARIFICATION → research task
   - For each dependency → best practices task
   - For each integration → patterns task

2. **Generate and dispatch research agents**:

   ```text
   For each unknown in Technical Context:
     Task: "Research {unknown} for {feature context}"
   For each technology choice:
     Task: "Find best practices for {tech} in {domain}"
   ```

3. **Consolidate findings** in `research.md` using format:
   - Decision: [what was chosen]
   - Rationale: [why chosen]
   - Alternatives considered: [what else evaluated]

**Output**: research.md with all NEEDS CLARIFICATION resolved

### Phase 1: Design & Contracts

**Prerequisites:** `research.md` complete

1. **Extract entities from feature spec** → `data-model.md`:
   - Entity name, fields, relationships
   - Validation rules from requirements
   - State transitions if applicable

2. **Generate API contracts** from functional requirements:
   - For each user action → endpoint
   - Use standard REST/GraphQL patterns
   - Output OpenAPI/GraphQL schema to `/contracts/`

**Output**: data-model.md, /contracts/*, quickstart.md

## Key rules

- Use absolute paths
- ERROR on gate failures or unresolved clarifications
