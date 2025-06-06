(module
  (type (func (result i32)))  ;; type #0
  (func (result i32) (i32.const 0))  ;; index 0
  (func (result i32) (i32.const 1))
  (func (result i32) (i32.const 2))
  (func (result i32) (i32.const 3))
  (func (result i32) (i32.const 4))  ;; index 4
  (table $t0 30 30 funcref)
  (table $t1 30 30 funcref)
  (elem (table $t0) (i32.const 2) func 3 1 4 1)
  (elem funcref
    (ref.func 2) (ref.func 7) (ref.func 1) (ref.func 8))
  (elem (table $t0) (i32.const 12) func 7 5 2 3 6)
  (elem funcref
    (ref.func 5) (ref.func 9) (ref.func 2) (ref.func 7) (ref.func 6))
  (elem (table $t1) (i32.const 3) func 1 3 1 4)
  (elem (table $t1) (i32.const 11) func 6 3 2 5 7)
  (func (result i32) (i32.const 5))  ;; index 5
  (func (result i32) (i32.const 6))
  (func (result i32) (i32.const 7))
  (func (result i32) (i32.const 8))
  (func (result i32) (i32.const 9))  ;; index 9
  (func (export "test")
    (table.copy $t0 $t0 (i32.const 13) (i32.const 2) (i32.const 3)))
  (func (export "check_t0") (param i32) (result i32)
    (call_indirect $t0 (type 0) (local.get 0)))
  (func (export "check_t1") (param i32) (result i32)
    (call_indirect $t1 (type 0) (local.get 0)))
)
