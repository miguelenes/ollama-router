# admin-job-cancel Specification

## Purpose
Lets an operator cancel a running durable pull/delete job instead of watching a wedged operation run to its timeout.

## Requirements

### Requirement: Operator cancel of a running job

The system SHALL expose `POST /router/v1/jobs/{id}/cancel` behind the fail-closed admin bearer. Cancelling a running job MUST prevent undispatched targets from starting, mark the job's incomplete targets with a terminal cancelled status, and make the job terminal and non-success. Watchers of the job's NDJSON stream MUST receive a terminal error line and MUST NOT receive a success line. Cancel reasons MUST be router-owned; upstream provider text MUST NOT appear in responses, streams, or logs. SQLite MUST keep operation metadata only. Cancelling an already terminal job MUST be 409 without changing it; an unknown job id MUST be 404.

#### Scenario: cancel ends a running pull

- **WHEN** an operator cancels a running fleet pull job
- **THEN** undispatched targets never start, the job becomes terminal non-success, and a pull NDJSON watcher sees a terminal error line and no success line

#### Scenario: terminal jobs cannot be cancelled

- **WHEN** an operator cancels a job that already finished successfully
- **THEN** the response is 409 and the job still reports success

#### Scenario: cancel is fail-closed

- **WHEN** `OLLAMA_ROUTER_ADMIN_TOKEN` is unset and a cancel request arrives
- **THEN** the response is 403 and the job is unchanged
