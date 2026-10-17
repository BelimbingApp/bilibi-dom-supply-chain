-- Purchasing 002: each company's purchase requests and purchase orders with
-- their lines, status history and amendments, as Bilimbi's
-- `20261012210000_create_supply_chain_purchasing` and
-- `20261013100000_allow_receipt_reversal` leave them together: a line's
-- received quantity may exceed the ordered one (the facade bounds it by the
-- tolerance the receiving caller states), and a closed order goes back to
-- partially received when a receipt against it is reversed.
--
-- Rows of one company reference each other through composite
-- `(id, company_id)` foreign keys, so an order cannot name another company's
-- supplier, tax code or request. The database numbers each company's
-- requests and orders 1, 2, 3, ... without gaps, and each order's amendments
-- likewise. Nothing is deleted: triggers refuse DELETE and TRUNCATE on every
-- table (a draft's lines aside), and status history rows and amendments are
-- append-only.
--
-- A request or order is free while it is a draft. From submission on, its
-- header changes only by a status transition the trigger allows, or (for an
-- approved or sent order) by an amendment that raises `amendment_count` by
-- one together with the amendable fields; its lines are frozen, except that
-- an amendment adds lines and supersedes the ones it replaces, and a receipt
-- changes a line's received quantity. A deferred constraint trigger refuses
-- a commit in which a request or order was created, changed status, or was
-- amended without the matching history or amendment row, so every transition
-- carries its actor, time and reason. The shared `refuse_change` function
-- comes from migration 001.

CREATE TABLE supply_chain_purchasing_requests (
  id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL,
  request_number bigint NOT NULL, status varchar(20) NOT NULL DEFAULT 'draft',
  title varchar(200) NOT NULL, justification varchar(1000), needed_on date,
  created_by_user_id bigint NOT NULL, created_by_impersonator_id bigint,
  submitted_by_user_id bigint, submitted_at timestamp(6),
  decided_by_user_id bigint, decided_at timestamp(6), decision_reason varchar(500),
  inserted_at timestamp(0) NOT NULL, updated_at timestamp(0) NOT NULL,
  CONSTRAINT supply_chain_purchasing_requests_id_company_unique UNIQUE (id, company_id),
  CONSTRAINT supply_chain_purchasing_requests_company_number_unique
    UNIQUE (company_id, request_number),
  CONSTRAINT supply_chain_purchasing_requests_request_number CHECK (request_number > 0),
  CONSTRAINT supply_chain_purchasing_requests_status
    CHECK (status IN ('draft', 'submitted', 'approved', 'rejected', 'cancelled')),
  CONSTRAINT supply_chain_purchasing_requests_lifecycle CHECK (
    (status = 'draft' AND submitted_by_user_id IS NULL AND submitted_at IS NULL
      AND decided_by_user_id IS NULL AND decided_at IS NULL AND decision_reason IS NULL)
    OR (status = 'submitted' AND submitted_by_user_id IS NOT NULL AND submitted_at IS NOT NULL
      AND decided_by_user_id IS NULL AND decided_at IS NULL AND decision_reason IS NULL)
    OR (status = 'approved' AND submitted_by_user_id IS NOT NULL AND submitted_at IS NOT NULL
      AND decided_by_user_id IS NOT NULL AND decided_at IS NOT NULL)
    OR (status = 'rejected' AND submitted_by_user_id IS NOT NULL AND submitted_at IS NOT NULL
      AND decided_by_user_id IS NOT NULL AND decided_at IS NOT NULL AND decision_reason IS NOT NULL)
    OR status = 'cancelled'
  )
);

CREATE INDEX supply_chain_purchasing_requests_tenant_company_status_index
  ON supply_chain_purchasing_requests (tenant_id, company_id, status);

CREATE TABLE supply_chain_purchasing_request_lines (
  id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL,
  request_id bigint NOT NULL, line_number integer NOT NULL,
  description varchar(500) NOT NULL, external_item_ref varchar(100),
  quantity numeric(20, 4) NOT NULL, unit varchar(20) NOT NULL,
  CONSTRAINT supply_chain_purchasing_request_lines_request_fkey
    FOREIGN KEY (request_id, company_id)
    REFERENCES supply_chain_purchasing_requests (id, company_id) ON DELETE RESTRICT,
  CONSTRAINT supply_chain_purchasing_request_lines_request_line_unique
    UNIQUE (request_id, line_number),
  CONSTRAINT supply_chain_purchasing_request_lines_line_number CHECK (line_number > 0),
  CONSTRAINT supply_chain_purchasing_request_lines_quantity CHECK (quantity > 0)
);

CREATE TABLE supply_chain_purchasing_request_events (
  id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL,
  request_id bigint NOT NULL, from_status varchar(20), to_status varchar(20) NOT NULL,
  actor_user_id bigint NOT NULL, actor_impersonator_id bigint, reason varchar(500),
  occurred_at timestamp(6) NOT NULL,
  CONSTRAINT supply_chain_purchasing_request_events_request_fkey
    FOREIGN KEY (request_id, company_id)
    REFERENCES supply_chain_purchasing_requests (id, company_id) ON DELETE RESTRICT
);

CREATE INDEX supply_chain_purchasing_request_events_request_id_id_index
  ON supply_chain_purchasing_request_events (request_id, id);

CREATE TABLE supply_chain_purchasing_orders (
  id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL,
  order_number bigint NOT NULL, purchase_request_id bigint, supplier_id bigint NOT NULL,
  currency_code varchar(3) NOT NULL, status varchar(20) NOT NULL DEFAULT 'draft',
  reference varchar(100), notes varchar(1000), expected_on date,
  amendment_count integer NOT NULL DEFAULT 0,
  created_by_user_id bigint NOT NULL, created_by_impersonator_id bigint,
  submitted_by_user_id bigint, submitted_at timestamp(6),
  approved_by_user_id bigint, approved_at timestamp(6),
  sent_at timestamp(6), closed_at timestamp(6), cancelled_at timestamp(6),
  inserted_at timestamp(0) NOT NULL, updated_at timestamp(0) NOT NULL,
  CONSTRAINT supply_chain_purchasing_orders_id_company_unique UNIQUE (id, company_id),
  CONSTRAINT supply_chain_purchasing_orders_company_number_unique
    UNIQUE (company_id, order_number),
  CONSTRAINT supply_chain_purchasing_orders_supplier_fkey
    FOREIGN KEY (supplier_id, company_id)
    REFERENCES supply_chain_purchasing_suppliers (id, company_id) ON DELETE RESTRICT,
  CONSTRAINT supply_chain_purchasing_orders_request_fkey
    FOREIGN KEY (purchase_request_id, company_id)
    REFERENCES supply_chain_purchasing_requests (id, company_id) ON DELETE RESTRICT,
  CONSTRAINT supply_chain_purchasing_orders_order_number CHECK (order_number > 0),
  CONSTRAINT supply_chain_purchasing_orders_amendment_count CHECK (amendment_count >= 0),
  CONSTRAINT supply_chain_purchasing_orders_currency_code
    CHECK (currency_code ~ '^[A-Z]{3}$'),
  CONSTRAINT supply_chain_purchasing_orders_status
    CHECK (status IN ('draft', 'submitted', 'approved', 'sent', 'partially_received',
      'closed', 'cancelled')),
  CONSTRAINT supply_chain_purchasing_orders_lifecycle CHECK (
    (status = 'draft' AND submitted_by_user_id IS NULL AND submitted_at IS NULL
      AND approved_by_user_id IS NULL AND approved_at IS NULL AND sent_at IS NULL
      AND closed_at IS NULL AND cancelled_at IS NULL AND amendment_count = 0)
    OR (status = 'submitted' AND submitted_by_user_id IS NOT NULL AND submitted_at IS NOT NULL
      AND approved_by_user_id IS NULL AND approved_at IS NULL AND sent_at IS NULL
      AND closed_at IS NULL AND cancelled_at IS NULL)
    OR (status = 'approved' AND submitted_by_user_id IS NOT NULL AND submitted_at IS NOT NULL
      AND approved_by_user_id IS NOT NULL AND approved_at IS NOT NULL AND sent_at IS NULL
      AND closed_at IS NULL AND cancelled_at IS NULL)
    OR (status IN ('sent', 'partially_received') AND submitted_by_user_id IS NOT NULL
      AND submitted_at IS NOT NULL AND approved_by_user_id IS NOT NULL AND approved_at IS NOT NULL
      AND sent_at IS NOT NULL AND closed_at IS NULL AND cancelled_at IS NULL)
    OR (status = 'closed' AND submitted_by_user_id IS NOT NULL AND submitted_at IS NOT NULL
      AND approved_by_user_id IS NOT NULL AND approved_at IS NOT NULL AND sent_at IS NOT NULL
      AND closed_at IS NOT NULL AND cancelled_at IS NULL)
    OR (status = 'cancelled' AND cancelled_at IS NOT NULL AND closed_at IS NULL)
  )
);

CREATE INDEX supply_chain_purchasing_orders_tenant_company_status_index
  ON supply_chain_purchasing_orders (tenant_id, company_id, status);

CREATE INDEX supply_chain_purchasing_orders_company_id_supplier_id_index
  ON supply_chain_purchasing_orders (company_id, supplier_id);

CREATE TABLE supply_chain_purchasing_order_amendments (
  id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL,
  order_id bigint NOT NULL, amendment_number integer NOT NULL, reason varchar(500) NOT NULL,
  amended_by_user_id bigint NOT NULL, amended_by_impersonator_id bigint,
  occurred_at timestamp(6) NOT NULL,
  CONSTRAINT supply_chain_purchasing_order_amendments_order_fkey
    FOREIGN KEY (order_id, company_id)
    REFERENCES supply_chain_purchasing_orders (id, company_id) ON DELETE RESTRICT,
  CONSTRAINT supply_chain_purchasing_order_amendments_id_order_unique UNIQUE (id, order_id),
  CONSTRAINT supply_chain_purchasing_order_amendments_order_number_unique
    UNIQUE (order_id, amendment_number),
  CONSTRAINT supply_chain_purchasing_order_amendments_amendment_number
    CHECK (amendment_number > 0)
);

CREATE TABLE supply_chain_purchasing_order_lines (
  id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL,
  order_id bigint NOT NULL, amendment_id bigint, superseded_by_amendment_id bigint,
  line_number integer NOT NULL, description varchar(500) NOT NULL,
  external_item_ref varchar(100), quantity numeric(20, 4) NOT NULL, unit varchar(20) NOT NULL,
  unit_price numeric(20, 4) NOT NULL, tax_code_id bigint, tax_rate numeric(7, 4),
  received_quantity numeric(20, 4) NOT NULL DEFAULT 0,
  CONSTRAINT supply_chain_purchasing_order_lines_order_fkey
    FOREIGN KEY (order_id, company_id)
    REFERENCES supply_chain_purchasing_orders (id, company_id) ON DELETE RESTRICT,
  CONSTRAINT supply_chain_purchasing_order_lines_amendment_fkey
    FOREIGN KEY (amendment_id, order_id)
    REFERENCES supply_chain_purchasing_order_amendments (id, order_id) ON DELETE RESTRICT,
  CONSTRAINT supply_chain_purchasing_order_lines_superseded_fkey
    FOREIGN KEY (superseded_by_amendment_id, order_id)
    REFERENCES supply_chain_purchasing_order_amendments (id, order_id) ON DELETE RESTRICT,
  CONSTRAINT supply_chain_purchasing_order_lines_tax_code_fkey
    FOREIGN KEY (tax_code_id, company_id)
    REFERENCES supply_chain_purchasing_tax_codes (id, company_id) ON DELETE RESTRICT,
  CONSTRAINT supply_chain_purchasing_order_lines_line_number CHECK (line_number > 0),
  CONSTRAINT supply_chain_purchasing_order_lines_quantity CHECK (quantity > 0),
  CONSTRAINT supply_chain_purchasing_order_lines_unit_price CHECK (unit_price >= 0),
  CONSTRAINT supply_chain_purchasing_order_lines_received_quantity
    CHECK (received_quantity >= 0),
  CONSTRAINT supply_chain_purchasing_order_lines_tax CHECK (
    (tax_code_id IS NULL AND tax_rate IS NULL)
    OR (tax_code_id IS NOT NULL AND tax_rate IS NOT NULL AND tax_rate >= 0 AND tax_rate <= 100)
  ),
  CONSTRAINT supply_chain_purchasing_order_lines_not_self_superseded
    CHECK (superseded_by_amendment_id IS NULL OR amendment_id IS NULL
      OR superseded_by_amendment_id <> amendment_id)
);

CREATE UNIQUE INDEX supply_chain_purchasing_order_lines_current_line_unique
  ON supply_chain_purchasing_order_lines (order_id, line_number)
  WHERE superseded_by_amendment_id IS NULL;

CREATE INDEX supply_chain_purchasing_order_lines_order_id_id_index
  ON supply_chain_purchasing_order_lines (order_id, id);

CREATE INDEX supply_chain_purchasing_order_lines_company_tax_code_index
  ON supply_chain_purchasing_order_lines (company_id, tax_code_id);

CREATE TABLE supply_chain_purchasing_order_events (
  id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL,
  order_id bigint NOT NULL, from_status varchar(20), to_status varchar(20) NOT NULL,
  actor_user_id bigint NOT NULL, actor_impersonator_id bigint, reason varchar(500),
  occurred_at timestamp(6) NOT NULL,
  CONSTRAINT supply_chain_purchasing_order_events_order_fkey
    FOREIGN KEY (order_id, company_id)
    REFERENCES supply_chain_purchasing_orders (id, company_id) ON DELETE RESTRICT
);

CREATE INDEX supply_chain_purchasing_order_events_order_id_id_index
  ON supply_chain_purchasing_order_events (order_id, id);

-- The database numbers each company's requests and orders 1, 2, 3, ...
-- whatever the inserting writer sent: with no sequence and no deletion, a
-- number is never skipped or reused, and a concurrent writer that takes the
-- same number fails on the unique constraint (the facade's company lock
-- keeps that from happening).
CREATE FUNCTION supply_chain_purchasing_number_request() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  NEW.request_number := COALESCE(
    (SELECT max(request_number) FROM supply_chain_purchasing_requests
     WHERE company_id = NEW.company_id), 0) + 1;
  RETURN NEW;
END
$$;

CREATE FUNCTION supply_chain_purchasing_number_order() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  NEW.order_number := COALESCE(
    (SELECT max(order_number) FROM supply_chain_purchasing_orders
     WHERE company_id = NEW.company_id), 0) + 1;
  RETURN NEW;
END
$$;

CREATE FUNCTION supply_chain_purchasing_number_amendment() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  order_status varchar(20);
BEGIN
  SELECT status INTO order_status FROM supply_chain_purchasing_orders WHERE id = NEW.order_id;

  IF order_status IS DISTINCT FROM 'approved' AND order_status IS DISTINCT FROM 'sent' THEN
    RAISE EXCEPTION 'purchase order % is %: only an approved or sent order is amended',
      NEW.order_id, order_status
      USING ERRCODE = 'restrict_violation';
  END IF;

  NEW.amendment_number := COALESCE(
    (SELECT max(amendment_number) FROM supply_chain_purchasing_order_amendments
     WHERE order_id = NEW.order_id), 0) + 1;
  NEW.occurred_at := clock_timestamp() AT TIME ZONE 'UTC';
  RETURN NEW;
END
$$;

CREATE FUNCTION supply_chain_purchasing_stamp_event() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  NEW.occurred_at := clock_timestamp() AT TIME ZONE 'UTC';
  RETURN NEW;
END
$$;

CREATE FUNCTION supply_chain_purchasing_request_header_rules() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.tenant_id <> OLD.tenant_id OR NEW.company_id <> OLD.company_id
    OR NEW.request_number <> OLD.request_number
    OR NEW.created_by_user_id <> OLD.created_by_user_id
    OR NEW.created_by_impersonator_id IS DISTINCT FROM OLD.created_by_impersonator_id THEN
    RAISE EXCEPTION 'a purchase request keeps its company, number and creator'
      USING ERRCODE = 'restrict_violation';
  END IF;

  IF NEW.status <> OLD.status THEN
    IF (OLD.status, NEW.status) NOT IN (('draft', 'submitted'), ('draft', 'cancelled'),
      ('submitted', 'approved'), ('submitted', 'rejected'), ('submitted', 'cancelled'),
      ('approved', 'cancelled')) THEN
      RAISE EXCEPTION 'a purchase request does not go from % to %', OLD.status, NEW.status
        USING ERRCODE = 'restrict_violation';
    END IF;
  ELSIF OLD.status <> 'draft' THEN
    RAISE EXCEPTION 'purchase request % is %: it changes only by a status transition',
      OLD.id, OLD.status
      USING ERRCODE = 'restrict_violation';
  END IF;

  IF OLD.status <> 'draft' AND (NEW.title <> OLD.title
    OR NEW.justification IS DISTINCT FROM OLD.justification
    OR NEW.needed_on IS DISTINCT FROM OLD.needed_on) THEN
    RAISE EXCEPTION 'purchase request % is %: its title, justification and date are fixed',
      OLD.id, OLD.status
      USING ERRCODE = 'restrict_violation';
  END IF;

  RETURN NEW;
END
$$;

CREATE FUNCTION supply_chain_purchasing_request_line_rules() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  request_status varchar(20);
  subject_id bigint;
BEGIN
  IF TG_OP = 'DELETE' THEN
    subject_id := OLD.request_id;
  ELSE
    subject_id := NEW.request_id;
  END IF;

  IF TG_OP = 'UPDATE' AND (NEW.tenant_id <> OLD.tenant_id OR NEW.company_id <> OLD.company_id
    OR NEW.request_id <> OLD.request_id) THEN
    RAISE EXCEPTION 'a purchase request line keeps its request'
      USING ERRCODE = 'restrict_violation';
  END IF;

  SELECT status INTO request_status FROM supply_chain_purchasing_requests WHERE id = subject_id;

  IF request_status IS DISTINCT FROM 'draft' THEN
    RAISE EXCEPTION 'purchase request % is %: its lines are fixed', subject_id, request_status
      USING ERRCODE = 'restrict_violation';
  END IF;

  IF TG_OP = 'DELETE' THEN
    RETURN OLD;
  END IF;

  RETURN NEW;
END
$$;

CREATE FUNCTION supply_chain_purchasing_order_header_rules() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.tenant_id <> OLD.tenant_id OR NEW.company_id <> OLD.company_id
    OR NEW.order_number <> OLD.order_number
    OR NEW.created_by_user_id <> OLD.created_by_user_id
    OR NEW.created_by_impersonator_id IS DISTINCT FROM OLD.created_by_impersonator_id THEN
    RAISE EXCEPTION 'a purchase order keeps its company, number and creator'
      USING ERRCODE = 'restrict_violation';
  END IF;

  IF OLD.status = 'draft' THEN
    IF NEW.status = OLD.status OR (OLD.status, NEW.status) IN (('draft', 'submitted'),
      ('draft', 'cancelled')) THEN
      RETURN NEW;
    END IF;

    RAISE EXCEPTION 'a purchase order does not go from % to %', OLD.status, NEW.status
      USING ERRCODE = 'restrict_violation';
  END IF;

  IF NEW.supplier_id <> OLD.supplier_id OR NEW.currency_code <> OLD.currency_code
    OR NEW.purchase_request_id IS DISTINCT FROM OLD.purchase_request_id THEN
    RAISE EXCEPTION 'purchase order % is %: its supplier, currency and request are fixed',
      OLD.id, OLD.status
      USING ERRCODE = 'restrict_violation';
  END IF;

  IF NEW.status <> OLD.status THEN
    IF (OLD.status, NEW.status) NOT IN (('submitted', 'approved'), ('submitted', 'draft'),
      ('submitted', 'cancelled'), ('approved', 'sent'), ('approved', 'submitted'),
      ('approved', 'cancelled'), ('sent', 'partially_received'), ('sent', 'closed'),
      ('sent', 'submitted'), ('sent', 'cancelled'), ('partially_received', 'closed'),
      ('partially_received', 'cancelled'), ('closed', 'partially_received')) THEN
      RAISE EXCEPTION 'a purchase order does not go from % to %', OLD.status, NEW.status
        USING ERRCODE = 'restrict_violation';
    END IF;

    IF NEW.amendment_count <> OLD.amendment_count THEN
      RAISE EXCEPTION 'a purchase order is not amended in the same change as a transition'
        USING ERRCODE = 'restrict_violation';
    END IF;
  ELSIF NEW.amendment_count = OLD.amendment_count + 1 THEN
    IF OLD.status NOT IN ('approved', 'sent') THEN
      RAISE EXCEPTION 'purchase order % is %: only an approved or sent order is amended',
        OLD.id, OLD.status
        USING ERRCODE = 'restrict_violation';
    END IF;

    RETURN NEW;
  ELSE
    RAISE EXCEPTION 'purchase order % is %: it changes only by a transition or an amendment',
      OLD.id, OLD.status
      USING ERRCODE = 'restrict_violation';
  END IF;

  IF NEW.reference IS DISTINCT FROM OLD.reference OR NEW.notes IS DISTINCT FROM OLD.notes
    OR NEW.expected_on IS DISTINCT FROM OLD.expected_on THEN
    RAISE EXCEPTION 'purchase order % is %: its reference, notes and date change only by an amendment',
      OLD.id, OLD.status
      USING ERRCODE = 'restrict_violation';
  END IF;

  RETURN NEW;
END
$$;

CREATE FUNCTION supply_chain_purchasing_order_line_rules() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  order_status varchar(20);
  subject_id bigint;
  expected supply_chain_purchasing_order_lines%ROWTYPE;
BEGIN
  IF TG_OP = 'DELETE' THEN
    subject_id := OLD.order_id;
  ELSE
    subject_id := NEW.order_id;
  END IF;

  SELECT status INTO order_status FROM supply_chain_purchasing_orders WHERE id = subject_id;

  IF TG_OP = 'DELETE' THEN
    IF order_status IS DISTINCT FROM 'draft' THEN
      RAISE EXCEPTION 'purchase order % is %: its lines are kept', subject_id, order_status
        USING ERRCODE = 'restrict_violation';
    END IF;

    RETURN OLD;
  END IF;

  IF TG_OP = 'INSERT' THEN
    IF NEW.superseded_by_amendment_id IS NOT NULL OR NEW.received_quantity <> 0 THEN
      RAISE EXCEPTION 'a new purchase order line is current and unreceived'
        USING ERRCODE = 'restrict_violation';
    END IF;

    IF order_status = 'draft' AND NEW.amendment_id IS NULL THEN
      RETURN NEW;
    END IF;

    IF order_status IN ('approved', 'sent') AND NEW.amendment_id IS NOT NULL THEN
      RETURN NEW;
    END IF;

    RAISE EXCEPTION 'purchase order % is %: a line is added to a draft, or by an amendment',
      subject_id, order_status
      USING ERRCODE = 'restrict_violation';
  END IF;

  IF NEW.tenant_id <> OLD.tenant_id OR NEW.company_id <> OLD.company_id
    OR NEW.order_id <> OLD.order_id THEN
    RAISE EXCEPTION 'a purchase order line keeps its order'
      USING ERRCODE = 'restrict_violation';
  END IF;

  IF order_status = 'draft' THEN
    IF NEW.amendment_id IS NOT NULL OR NEW.superseded_by_amendment_id IS NOT NULL
      OR NEW.received_quantity <> 0 THEN
      RAISE EXCEPTION 'a draft purchase order line is current and unreceived'
        USING ERRCODE = 'restrict_violation';
    END IF;

    RETURN NEW;
  END IF;

  expected := OLD;
  expected.received_quantity := NEW.received_quantity;

  IF NEW IS NOT DISTINCT FROM expected THEN
    IF order_status IN ('sent', 'partially_received') AND OLD.superseded_by_amendment_id IS NULL THEN
      RETURN NEW;
    END IF;

    RAISE EXCEPTION 'purchase order % is %: a receipt is recorded against a sent order',
      subject_id, order_status
      USING ERRCODE = 'restrict_violation';
  END IF;

  expected := OLD;
  expected.superseded_by_amendment_id := NEW.superseded_by_amendment_id;

  IF NEW IS NOT DISTINCT FROM expected AND OLD.superseded_by_amendment_id IS NULL
    AND NEW.superseded_by_amendment_id IS NOT NULL THEN
    IF order_status IN ('approved', 'sent') THEN
      RETURN NEW;
    END IF;

    RAISE EXCEPTION 'purchase order % is %: only an approved or sent order is amended',
      subject_id, order_status
      USING ERRCODE = 'restrict_violation';
  END IF;

  RAISE EXCEPTION 'purchase order % is %: its lines change only by an amendment or a receipt',
    subject_id, order_status
    USING ERRCODE = 'restrict_violation';
END
$$;

-- Every creation and transition carries a history row written in the same
-- transaction. now() is the transaction's start; events are stamped with
-- clock_timestamp(), so a row from an earlier transaction never counts.
CREATE FUNCTION supply_chain_purchasing_request_audited() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  previous varchar(20);
BEGIN
  IF TG_OP = 'UPDATE' THEN
    IF NEW.status = OLD.status THEN
      RETURN NULL;
    END IF;

    previous := OLD.status;
  END IF;

  IF NOT EXISTS (
    SELECT 1 FROM supply_chain_purchasing_request_events
    WHERE request_id = NEW.id AND from_status IS NOT DISTINCT FROM previous
      AND to_status = NEW.status AND occurred_at >= now() AT TIME ZONE 'UTC'
  ) THEN
    RAISE EXCEPTION 'purchase request % became % without a history row', NEW.id, NEW.status
      USING ERRCODE = 'check_violation';
  END IF;

  RETURN NULL;
END
$$;

CREATE FUNCTION supply_chain_purchasing_order_audited() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  previous varchar(20);
BEGIN
  IF TG_OP = 'UPDATE' AND NEW.amendment_count <> OLD.amendment_count THEN
    IF NOT EXISTS (
      SELECT 1 FROM supply_chain_purchasing_order_amendments
      WHERE order_id = NEW.id AND amendment_number = NEW.amendment_count
        AND occurred_at >= now() AT TIME ZONE 'UTC'
    ) THEN
      RAISE EXCEPTION 'purchase order % counts amendment % without its record',
        NEW.id, NEW.amendment_count
        USING ERRCODE = 'check_violation';
    END IF;
  END IF;

  IF TG_OP = 'UPDATE' THEN
    IF NEW.status = OLD.status THEN
      RETURN NULL;
    END IF;

    previous := OLD.status;

    IF previous IN ('approved', 'sent') AND NEW.status = 'submitted' THEN
      IF NOT EXISTS (
        SELECT 1 FROM supply_chain_purchasing_order_amendments
        WHERE order_id = NEW.id AND amendment_number = NEW.amendment_count
          AND occurred_at >= now() AT TIME ZONE 'UTC'
      ) THEN
        RAISE EXCEPTION 'purchase order % went back to submitted without an amendment', NEW.id
          USING ERRCODE = 'check_violation';
      END IF;
    END IF;
  END IF;

  IF NOT EXISTS (
    SELECT 1 FROM supply_chain_purchasing_order_events
    WHERE order_id = NEW.id AND from_status IS NOT DISTINCT FROM previous
      AND to_status = NEW.status AND occurred_at >= now() AT TIME ZONE 'UTC'
  ) THEN
    RAISE EXCEPTION 'purchase order % became % without a history row', NEW.id, NEW.status
      USING ERRCODE = 'check_violation';
  END IF;

  RETURN NULL;
END
$$;

CREATE TRIGGER supply_chain_purchasing_requests_no_truncate
  BEFORE TRUNCATE ON supply_chain_purchasing_requests
  FOR EACH STATEMENT EXECUTE FUNCTION supply_chain_purchasing_refuse_change();
CREATE TRIGGER supply_chain_purchasing_request_lines_no_truncate
  BEFORE TRUNCATE ON supply_chain_purchasing_request_lines
  FOR EACH STATEMENT EXECUTE FUNCTION supply_chain_purchasing_refuse_change();
CREATE TRIGGER supply_chain_purchasing_request_events_no_truncate
  BEFORE TRUNCATE ON supply_chain_purchasing_request_events
  FOR EACH STATEMENT EXECUTE FUNCTION supply_chain_purchasing_refuse_change();
CREATE TRIGGER supply_chain_purchasing_orders_no_truncate
  BEFORE TRUNCATE ON supply_chain_purchasing_orders
  FOR EACH STATEMENT EXECUTE FUNCTION supply_chain_purchasing_refuse_change();
CREATE TRIGGER supply_chain_purchasing_order_amendments_no_truncate
  BEFORE TRUNCATE ON supply_chain_purchasing_order_amendments
  FOR EACH STATEMENT EXECUTE FUNCTION supply_chain_purchasing_refuse_change();
CREATE TRIGGER supply_chain_purchasing_order_lines_no_truncate
  BEFORE TRUNCATE ON supply_chain_purchasing_order_lines
  FOR EACH STATEMENT EXECUTE FUNCTION supply_chain_purchasing_refuse_change();
CREATE TRIGGER supply_chain_purchasing_order_events_no_truncate
  BEFORE TRUNCATE ON supply_chain_purchasing_order_events
  FOR EACH STATEMENT EXECUTE FUNCTION supply_chain_purchasing_refuse_change();

CREATE TRIGGER supply_chain_purchasing_requests_kept
  BEFORE DELETE ON supply_chain_purchasing_requests
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_refuse_change();
CREATE TRIGGER supply_chain_purchasing_orders_kept
  BEFORE DELETE ON supply_chain_purchasing_orders
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_refuse_change();

CREATE TRIGGER supply_chain_purchasing_request_events_append_only
  BEFORE UPDATE OR DELETE ON supply_chain_purchasing_request_events
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_refuse_change();
CREATE TRIGGER supply_chain_purchasing_order_amendments_append_only
  BEFORE UPDATE OR DELETE ON supply_chain_purchasing_order_amendments
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_refuse_change();
CREATE TRIGGER supply_chain_purchasing_order_events_append_only
  BEFORE UPDATE OR DELETE ON supply_chain_purchasing_order_events
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_refuse_change();

CREATE TRIGGER supply_chain_purchasing_requests_number
  BEFORE INSERT ON supply_chain_purchasing_requests
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_number_request();
CREATE TRIGGER supply_chain_purchasing_orders_number
  BEFORE INSERT ON supply_chain_purchasing_orders
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_number_order();
CREATE TRIGGER supply_chain_purchasing_order_amendments_number
  BEFORE INSERT ON supply_chain_purchasing_order_amendments
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_number_amendment();
CREATE TRIGGER supply_chain_purchasing_request_events_stamp
  BEFORE INSERT ON supply_chain_purchasing_request_events
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_stamp_event();
CREATE TRIGGER supply_chain_purchasing_order_events_stamp
  BEFORE INSERT ON supply_chain_purchasing_order_events
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_stamp_event();
CREATE TRIGGER supply_chain_purchasing_requests_header
  BEFORE UPDATE ON supply_chain_purchasing_requests
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_request_header_rules();
CREATE TRIGGER supply_chain_purchasing_request_lines_rules
  BEFORE INSERT OR UPDATE OR DELETE ON supply_chain_purchasing_request_lines
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_request_line_rules();
CREATE TRIGGER supply_chain_purchasing_orders_header
  BEFORE UPDATE ON supply_chain_purchasing_orders
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_order_header_rules();
CREATE TRIGGER supply_chain_purchasing_order_lines_rules
  BEFORE INSERT OR UPDATE OR DELETE ON supply_chain_purchasing_order_lines
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_order_line_rules();

CREATE CONSTRAINT TRIGGER supply_chain_purchasing_requests_audited
  AFTER INSERT OR UPDATE ON supply_chain_purchasing_requests
  DEFERRABLE INITIALLY DEFERRED
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_request_audited();
CREATE CONSTRAINT TRIGGER supply_chain_purchasing_orders_audited
  AFTER INSERT OR UPDATE ON supply_chain_purchasing_orders
  DEFERRABLE INITIALLY DEFERRED
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_order_audited();
