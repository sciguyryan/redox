#[repr(u16)]
pub enum OpCode {
    /// Subroutine - a pseudo-opcode used to identify a subroutine position.
    //Subroutine = -2,
    /// Label - a pseudo-opcode used to identify a labels position.
    //Label = -1,

    /// No Operation - a non-operation instruction.
    Nop = 0,

    /// Add u32 Literal to Register.
    AddU32LitReg = 1,

    /// Machine return - downgrade the privilege level of the processor.
    Mret = 65534,

    /// Halt - halt the execution of the virtual machine.
    Hlt = 65535,
}
