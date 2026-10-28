"""Synthetic fixed-protocol plugin; never opens a database or network socket."""
import hashlib
import json
import os
import sys
import threading
import time

lock = threading.RLock()
settings = {}
initializations = 0
queries = 0
active = {}
identities = []
sessions = set()
invalidations = []


def emit(request, result=None, error=None):
    response = {"jsonrpc": "2.0", "id": request["id"]}
    response["error" if error is not None else "result"] = error if error is not None else result
    with lock:
        sys.stdout.write(json.dumps(response, separators=(",", ":")) + "\n")
        sys.stdout.flush()


def rejected(request, code="INVALID_ARGUMENT", outcome="not_started", closed=False):
    emit(request, error={"code": -32000, "message": "SECRET_CANARY_REMOTE", "data": {
        "code": code, "message": "SECRET_CANARY_REMOTE", "retryable": False,
        "outcome": outcome, "details": {"session_closed": closed, "secret": "SECRET_CANARY_REMOTE"}}})


def serve(request):
    global settings, initializations, queries
    method = request["method"]
    wire = request.get("params", {})
    if method == "initialize":
        with lock:
            initializations += 1
            settings = wire.get("settings", {})
        mode = settings.get("mode", "native")
        if settings.get("slow_first_initialize") and initializations == 1:
            time.sleep(0.1)
        if mode == "init_error":
            rejected(request)
            return
        if mode == "missing":
            emit(request, error={"code": -32601, "message": "legacy missing"})
            return
        if mode == "legacy":
            emit(request)
            return
        if mode in ("cosmos", "cosmos_optin"):
            if mode == "cosmos" and set(wire) != {"settings"}:
                rejected(request)
                return
            caps = {"protocol_version": 1, "documents_v1": True, "query_page_v1": True,
                    "cancel_v1": True, "sessions_v1": False}
        else:
            caps = {"service_protocol": 2 if mode == "protocol2" else 1, "exact_query": True,
                    "spatial_v1": True, "sessions": True, "cancel_v1": True,
                    "catalog": True, "rows": True, "exact_affected_rows": mode != "wrapped",
                    "readonly_guarantee": "restricted_database_principal" if mode == "wrapped" else "read_only_transaction"}
        result = {"service_capabilities": caps}
        emit(request, {"result": result} if mode == "wrapped" else result)
        return
    if method == "get_tables" and "driver_context" not in wire:
        emit(request, [])
        return
    if method == "cancel_request":
        with lock:
            target = active.get((wire["connection_id"], wire["request_id"]))
            if target is not None:
                target.set()
        emit(request, {"cancel_requested": True})
        return
    if method == "fixture_stats":
        emit(request, {"initializations": initializations, "queries": queries, "active": [list(key) for key in active]})
        return
    context = wire.get("driver_context", {})
    input_value = wire.get("input", {})
    auth = context.get("auth")
    if auth:
        with lock:
            identities.append(auth["identity"])
    if method == "service_test":
        with lock:
            emit(request, {"initializations": initializations, "queries": queries,
                           "active": [list(key) for key in active], "identities": list(identities),
                           "sessions": len(sessions), "invalidations": list(invalidations)})
        return
    if method in ("spatial_columns", "spatial_transform_values", "spatial_query_table", "spatial_read_feature"):
        if input_value.get("probe") == "wait":
            event = threading.Event()
            key = (context["connection_id"], context["request_id"])
            with lock:
                active[key] = event
            event.wait(1)
            with lock:
                active.pop(key, None)
            if event.is_set():
                rejected(request, "CANCELLED", "not_applied")
                return
        emit(request, {"database": wire["params"]["database"], "read_only": context["read_only"]})
        return
    if method == "service_session_open":
        if settings.get("slow_session"):
            time.sleep(0.15)
        handle = "physical-" + context["request_id"]
        with lock:
            sessions.add(handle)
        emit(request, {"session_handle": handle, "in_transaction": not settings.get("invalid_session_response"), "rollback_confirmed": False})
        return
    if method in ("service_session_finish", "service_session_discard"):
        with lock:
            sessions.discard(context["session_handle"])
        emit(request, {"session_handle": context["session_handle"], "in_transaction": False,
                       "rollback_confirmed": method == "service_session_finish" and input_value["action"] == "rollback" and not settings.get("unconfirmed_rollback")})
        return
    if method == "service_invalidate_auth":
        with lock:
            minimal = wire["params"].get("host") is None and wire["params"].get("password") is None and not wire["params"].get("extra") and "auth" not in context and "session_handle" not in context
            invalidations.append({"connection_id": context["connection_id"], "minimal": minimal})
        emit(request, {})
        return
    if method in ("service_catalog", "service_row"):
        emit(request, {"operation": input_value["operation"], "database": wire["params"]["database"]})
        return
    if method == "get_columns":
        emit(request, {"columns": [], "partition_key_paths": ["/region", "/id"],
                       "partition_key_kind": "MultiHash", "partition_key_version": 2, "system_key": False})
        return
    if method in ("execute_query", "query_page"):
        query = input_value.get("query", input_value)
        cursor = input_value.get("cursor_state")
        if cursor and cursor["native_state"]["continuation_token"] != "PRIVATE_SDK_CURSOR":
            rejected(request)
            return
        result = {"data": {"kind": "json_values", "values": [None, True]},
                  "snapshot": {"sets": [{"kind": "json_values", "columns": [], "rows": [None, True]}],
                               "revision": "native-revision", "limits": {"truncated": False, "reasons": []},
                               "allow_truncation": False, "warnings": []},
                  "cursor": None if cursor else {"binding": {"sdk_version": "4.10.1", "container_rid": "container-rid",
                      "endpoint_identity": "endpoint-hash", "partition_hash": "partition-hash"},
                      "native_state": {"query": query, "continuation_token": "PRIVATE_SDK_CURSOR"},
                      "tail": ["private-tail"], "columns": [], "page_size": 2,
                      "expires_at_ms": int(time.time() * 1000) + 900000},
                  "metrics": {"elapsed_ms": 0, "request_charge": 2.5, "retry_count": 0}, "limits": {"truncated": False, "reasons": []},
                  "warnings": settings.get("continuation_warnings", []) if cursor else [], "confirmed_cancelled": False, "session_closed": False}
        emit(request, {"protocol_version": 1, "request_id": context["request_id"],
                       "connection_id": context["connection_id"], "status": "succeeded", "error": None,
                       "data": result, "page": {"next_token": None, "has_more": False, "resume_mode": "none"},
                       "result_id": None, "job_id": None, "metrics": {"elapsed_ms": 0, "request_charge": 2.5, "retry_count": 0},
                       "limits": {"truncated": False, "reasons": []}, "warnings": []})
        return
    if method != "execute_query_exact":
        emit(request, error={"code": -32601, "message": "unsupported synthetic method"})
        return
    with lock:
        queries += 1
    text = input_value["text"]
    if text == "transport_eof":
        os._exit(0)
    if text == "transport_utf8":
        with lock:
            sys.stdout.buffer.write(b"\xff\n")
            sys.stdout.buffer.flush()
        return
    if text == "transport_malformed":
        with lock:
            sys.stdout.write("not-json\n")
            sys.stdout.flush()
        return
    if text == "transport_oversized":
        with lock:
            sys.stdout.write("x" * (16 * 1024 * 1024 + 1) + "\n")
            sys.stdout.flush()
        return
    if text == "structured_error":
        rejected(request, closed=True)
        return
    if text in ("wait_pair", "wait_ack", "wait_timeout"):
        event = threading.Event()
        key = (context["connection_id"], context["request_id"])
        with lock:
            active[key] = event
        if text == "wait_pair":
            event.wait(0.5)
        else:
            event.wait(5)
        with lock:
            active.pop(key, None)
        if text == "wait_pair" and event.is_set():
            rejected(request, "CANCELLED", "not_applied")
            return
        if text == "wait_timeout":
            return
    expected = settings.get("expected_text", text)
    valid = (text == expected and input_value["parameters"] == settings.get("expected_parameters", input_value["parameters"])
             and set(input_value) == {"text", "parameters", "max_rows", "max_bytes"}
             and context.get("protocol_version") == 1 and 0 < context.get("deadline_ms", 0) <= 3600000)
    emit(request, {"columns": ["duplicate", "duplicate"], "column_metadata": [
        {"type": "geometry", "srid": 4326, "dimensions": "XY"}, {"type": "decimal", "precision": None}],
        "rows": [] if text == "zero_rows" else [[valid, None]], "affected_rows": None,
        "additional_results": [{"columns": ["empty"], "column_metadata": [{"type": "text"}], "rows": []}],
        "truncated": settings.get("truncated", False), "warnings": ["decimal_precision_unavailable"] * settings.get("metadata_count", 1),
        "source_sql": "SECRET_CANARY_REMOTE"})


for line in sys.stdin:
    request = json.loads(line)
    if "id" in request:
        threading.Thread(target=serve, args=(request,), daemon=True).start()
