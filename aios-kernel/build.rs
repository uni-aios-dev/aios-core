use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let error_vectors = [8u16, 10, 11, 12, 13, 14, 17, 21];
    let mut asm = String::new();
    for vector in 0..=255u16 {
        asm.push_str(&format!(".hidden aios_handler_{vector}\n"));
        asm.push_str(&format!(".global aios_handler_{vector}\n"));
        asm.push_str(&format!("aios_handler_{vector}:\n"));
        if error_vectors.contains(&vector) {
            asm.push_str(&format!("    push {vector}\n"));
        } else {
            asm.push_str("    push 0\n");
            asm.push_str(&format!("    push {vector}\n"));
        }
        asm.push_str("    jmp aios_interrupt_common\n");
    }
    asm.push_str(
        r#"
aios_interrupt_common:
    push rax
    mov rax, ds
    push rax
    mov rax, 0x10
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    push rcx
    push rdx
    push rbx
    push rbp
    push rsi
    push rdi
    mov rbx, rsp
    and rsp, -16
    mov rdi, rbx
    call aios_handle_interrupt
    mov rsp, rbx
    pop rdi
    pop rsi
    pop rbp
    pop rbx
    pop rdx
    pop rcx
    pop rax
    mov ds, rax
    pop rax
    add rsp, 16
    iretq

.section .data.rel.ro
.p2align 3
.hidden aios_handler_table
.global aios_handler_table
aios_handler_table:
"#,
    );
    for vector in 0..=255u16 {
        asm.push_str(&format!("    .quad aios_handler_{vector}\n"));
    }
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("irq_stubs.S");
    fs::write(&out, asm).expect("failed to write irq_stubs.S");
    println!("cargo:rerun-if-changed=build.rs");
}
