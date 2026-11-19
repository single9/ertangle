# termade

A terminal-native multi-agent environment: a human and several AI agents share one daemon to split work, exchange messages, and hand work over to each other.

## Language

**Session**:
A single running instance of an agent, with its own id, label, working directory, and position in the collaboration tree.
_Avoid_: window, tab

**Agent kind**:
The CLI program that runs a session (opencode, claude, codex, antigravity). It says nothing about the duty the session performs.
_Avoid_: "agent" on its own (easily confused with a session), model

**Role**:
The duty a session performs in the collaboration: a coordinator splits and directs work, a worker edits files, a reviewer reviews read-only.
_Avoid_: "identity" on its own

**Parent**:
The session that created this session and receives its reports. A session without a parent reports directly to the user.

**Identity**:
A session's role together with its position in the collaboration tree (who its parent is). Changing identity means changing both.

**Handoff**:
Stopping a session and letting a new session continue in the same working directory, starting from a handoff brief. A handoff may change the agent kind, the model, and the identity.
_Avoid_: resume (that is the same session continuing)

**Resume**:
Letting the same stopped session continue. Its agent kind and identity stay unchanged.

**Successor**:
The session that takes over after a handoff. Messages addressed to the old session are forwarded to it.

## Relationships

- A **Session** has exactly one **Agent kind** and one **Role**, and at most one **Parent**
- A **Handoff** produces a **Successor**; reports from the old session's children reach it through the successor chain
- **Identity** changes only through a **Handoff** the user triggers explicitly; the automatic handoff on quota exhaustion changes only the **Agent kind**
- A session that becomes a coordinator has no **Parent**; neither does a session the user creates directly

## Flagged ambiguities

- "Identity" was used to mean only the role. Resolved: identity = role + parent.
- "Resume" and "handoff" were used interchangeably. Resolved: only a handoff can change the agent kind or the identity.
