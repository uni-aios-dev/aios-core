use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let error_vectors = [8u16, 10, 11, 12, 13, 14, 17, 21];
    let mut asm = String::new();
    for vector in 0..=255u16 {
        asm.push_str(&format!(".globl aios_handler_{vector}\n"));
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
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    mov rbx, rsp
    and rsp, -16
    mov rdi, rbx
    call aios_handle_interrupt
    mov rsp, rbx
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
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
.globl aios_handler_table
aios_handler_table:
"#,
    );
    for vector in 0..=255u16 {
        asm.push_str(&format!("    .quad aios_handler_{vector}\n"));
    }
    asm.push_str(
        r#"
.section .text
.globl aios_restore_ring0
aios_restore_ring0:
    cli
    mov rax, [rdi + 112]
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov r15, [rdi + 0]
    mov r14, [rdi + 8]
    mov r13, [rdi + 16]
    mov r12, [rdi + 24]
    mov r11, [rdi + 32]
    mov r10, [rdi + 40]
    mov r9, [rdi + 48]
    mov r8, [rdi + 56]
    mov rax, [rdi + 120]
    mov rbx, [rdi + 88]
    mov rbp, [rdi + 80]
    mov rsi, [rdi + 72]
    mov rcx, [rdi + 104]
    mov rdx, [rdi + 96]
    mov rsp, [rdi + 168]
    mov r10, [rdi + 160]
    push r10
    popfq
mov r10, [rdi + 144]
mov rdi, [rdi + 64]
jmp r10

.section .text
.globl aios_restore_ring3
aios_restore_ring3:
    cli
    mov rax, [rdi + 112]
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov r15, [rdi + 0]
    mov r14, [rdi + 8]
    mov r13, [rdi + 16]
    mov r12, [rdi + 24]
    mov r11, [rdi + 32]
    mov r10, [rdi + 40]
    mov r9, [rdi + 48]
    mov r8, [rdi + 56]
    mov rax, [rdi + 120]
    mov rbx, [rdi + 88]
    mov rbp, [rdi + 80]
    mov rsi, [rdi + 72]
    mov rcx, [rdi + 104]
    mov rdx, [rdi + 96]
    mov rsp, [rdi + 168]
    mov r10, [rdi + 176]
    push r10
    mov r10, [rdi + 168]
    push r10
    mov r10, [rdi + 160]
    push r10
    mov r10, [rdi + 152]
    push r10
    mov r10, [rdi + 144]
    push r10
    iretq

.section .data.rel.ro
.p2align 3
.globl aios_handler_table
aios_handler_table:
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("irq_stubs.S");
    fs::write(&out, asm).expect("failed to write irq_stubs.S");
    println!("cargo:rerun-if-changed=build.rs");
}
