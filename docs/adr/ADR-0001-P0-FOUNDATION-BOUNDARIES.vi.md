# ADR-0001 — Ranh giới nền tảng P0

[English](ADR-0001-P0-FOUNDATION-BOUNDARIES.en.md) | Tiếng Việt

Trạng thái: đã hiện thực trong P0 ngày 10/09/2026.

## Bối cảnh

Harness cần contracts bền vững, kiểm tra được trước khi có thể lưu session an toàn, gọi model, thực thi tool hoặc giao việc. Kiến trúc chủ ý tách execution journal tương lai, `WorkingState` và reusable memory. Một runtime tiện tay trong P0 sẽ làm mờ ranh giới ownership đó và khiến các tuyên bố recovery sau này không đáng tin.

## Quyết định

P0 chỉ hiện thực Rust workspace, contracts có kiểu/version, ID và validation nghiêm ngặt, hash `harness-json-v1`, schemas sinh từ types thật, continuation oracle tĩnh, CLI `ha` tối thiểu không cần credential, acceptance registry và local/CI gate fail-closed.

Các luật ranh giới đã chấp nhận:

- Record chỉ nhận `schema_version = 1`; đổi nghĩa không tương thích cần revision mới.
- ID có kiểu `<kind>_<uuid-v7-lowercase-hyphenated>`; authority phải tường minh, không suy đoán.
- Canonical hashing không nhận số float hay exponent. Quy tắc số thực tương lai cần revision schema/contract được rà soát.
- `harness-types` chỉ chứa contracts và validation. Nó không sở hữu policy, persistence, model calls, tools, memory extraction hay orchestration.
- Fixture là oracle tĩnh độc lập, không phải runtime projector hoặc tuyên bố rằng resume đã được hiện thực.
- P0 không có session store, SQLite migration, model/provider, tool execution, memory runtime, multi-agent runtime, Web API/UI, release package hay bảo đảm compatibility công khai.

## Hệ quả

P1 phải thêm kernel/session/SQLite phía sau contracts đã chấp nhận này và không được đổi nghĩa âm thầm record P0. P2–P8 vẫn là assignment riêng, không được suy ra là đã có chỉ vì schema types đã tồn tại. Caller có thể inspect hoặc validate dữ liệu P0, nhưng không thể chỉ dùng P0 để resume một agent session đang sống.

Schema files được tái sinh bằng binary `generate_schemas` đã kiểm tra trong repo và được so sánh byte-for-byte với schemas sinh từ types thật trong process. Generator là maintenance tool, không phải contract authority thứ hai.

## Ranh giới kiểm chứng

Evidence P0 ghi command chính xác, dependency lock, test component thật, self-test của gate và manual defense mutations. CI đã được khai báo cho Windows/Linux nhưng chưa chạy cho đến khi có commit/push được giao riêng. Phase phải được nghiệm thu trước khi giao P1.
