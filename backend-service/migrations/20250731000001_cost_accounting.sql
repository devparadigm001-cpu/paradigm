CREATE TABLE IF NOT EXISTS execution_cost_events (
    id           UUID             PRIMARY KEY DEFAULT gen_random_uuid(),
    recorded_at  TIMESTAMPTZ      NOT NULL DEFAULT now(),
    device_id    TEXT             NOT NULL,
    user_id      TEXT,
    feature      TEXT             NOT NULL,
    model_source TEXT             NOT NULL,
    cost_usd     DOUBLE PRECISION NOT NULL,
    billable     BOOLEAN          NOT NULL
);

-- Primary access pattern: all events for a device in a time window
CREATE INDEX idx_ece_device_recorded ON execution_cost_events (device_id, recorded_at);

-- Secondary access pattern: all events for a user in a time window
CREATE INDEX idx_ece_user_recorded ON execution_cost_events (user_id, recorded_at)
    WHERE user_id IS NOT NULL;
