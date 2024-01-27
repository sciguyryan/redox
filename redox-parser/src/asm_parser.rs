use std::{collections::HashMap, num::ParseIntError, str::FromStr};

use itertools::Itertools;
use redox_core::{
    ins::{
        expressions::{
            Expression,
            {ExpressionArgs::*, ExpressionOperator::*},
        },
        instruction::Instruction,
        op_codes::OpCode,
    },
    reg::registers::RegisterId,
};

use crate::type_hints::{ArgTypeHint, InstructionLookup};

use super::type_hints::InstructionHints;

const F32_REGISTERS: [RegisterId; 2] = [RegisterId::FR1, RegisterId::FR2];

const U32_REGISTERS: [RegisterId; 17] = [
    RegisterId::ER1,
    RegisterId::ER2,
    RegisterId::ER3,
    RegisterId::ER4,
    RegisterId::ER5,
    RegisterId::ER6,
    RegisterId::ER7,
    RegisterId::ER8,
    RegisterId::EIP,
    RegisterId::EBP,
    RegisterId::ESP,
    RegisterId::EFL,
    RegisterId::EIM,
    RegisterId::IDTR,
    RegisterId::ESS,
    RegisterId::ECS,
    RegisterId::EDS,
];

/// A dummy label jump address used when handling a label.
const DUMMY_LABEL_JUMP_ADDRESS: u128 = u32::MAX as u128;

#[derive(Debug, Clone)]
pub enum Argument {
    /// A f64 argument.
    Float(f64),
    /// A u128 argument.
    UnsignedInt(u128),
    /// A f32 register argument.
    RegisterF32(RegisterId),
    /// A u32 register argument.
    RegisterU32(RegisterId),
    /// An expression argument.
    Expression(Expression),
}

/// Cheekily get the inner value of an enum.
macro_rules! get_inner_arg {
    ($target:expr, $enum:path) => {{
        if let $enum(a) = $target {
            a
        } else {
            unreachable!();
        }
    }};
}

/// Cheekily get the inner value of an enum, with casting.
macro_rules! get_inner_arg_and_cast {
    ($target:expr, $enum:path, $cast:ident) => {{
        if let $enum(a) = $target {
            a as $cast
        } else {
            unreachable!();
        }
    }};
}

/// Cheekily get and pack an inner enum expression.
macro_rules! get_inner_expr_arg {
    ($target:expr) => {{
        if let Argument::Expression(expr) = &$target {
            expr.pack()
        } else {
            unreachable!()
        }
    }};
}

pub struct AsmParser<'a> {
    /// The instruction hints for the parser.
    hints: InstructionHints<'a>,

    /// A vector of the parsed instructions.
    pub parsed_instructions: Vec<Instruction>,

    /// A vector containing any label hints that were encountered.
    pub labeled_instructions: HashMap<usize, (usize, String)>,
}

impl<'a> AsmParser<'a> {
    pub fn new() -> Self {
        Self {
            hints: InstructionHints::new(),
            parsed_instructions: vec![],
            labeled_instructions: HashMap::new(),
        }
    }

    /// Parse an assembly file.
    ///
    /// # Arguments
    ///
    /// * `string` - The string to be parsed.
    pub fn parse(&mut self, string: &str) {
        // Clear the list of label instruction hints.
        self.labeled_instructions = HashMap::with_capacity(string.lines().count());

        // Split the string into lines.
        let mut instructions = vec![];
        for line in string.lines().filter(|l| !l.starts_with(';')) {
            instructions.push(self.parse_code_line(line));
        }

        self.parsed_instructions = instructions;
    }

    /// Parse a code line of an assembly file.
    ///
    /// # Arguments
    ///
    /// * `line` - A code line to be parsed.
    ///
    /// # Returns
    ///
    /// A parsed [`Instruction`] instance.
    pub fn parse_code_line(&mut self, line: &str) -> Instruction {
        let raw_args = AsmParser::parse_instruction_line(line.trim_matches(' '));

        // Are we dealing with a label marker?
        // These are found at the start of a line and act as a target for branching instructions.
        if raw_args[0].starts_with(':') {
            if raw_args[0].len() == 1 {
                panic!("invalid syntax - a label designator without a name!");
            }

            return Instruction::Label(raw_args[0].to_string());
        }

        // The name should always be the first argument we've extracted, the arguments should follow.
        let name = raw_args[0].to_lowercase();

        // Used to hold the parsed arguments and the hints for the argument types.
        let mut arguments = vec![];
        let mut argument_hints = vec![];

        let shortlist: Vec<InstructionLookup> = self
            .hints
            .hints
            .iter()
            .filter(|h| h.names.contains(&name.as_str()))
            .cloned()
            .collect();

        assert!(
            !shortlist.is_empty(),
            "unable to find an instruction that matches the name."
        );

        // Do we have any arguments to process.
        for (i, raw_arg) in raw_args.iter().enumerate().skip(1) {
            let mut value_found = false;
            let mut hints = vec![];

            // This will track whether the argument is a pointer.
            let is_pointer = raw_arg.chars().nth(0).unwrap() == '&';

            // Skip past the address prefix identifier, if it exists.
            let substring = if is_pointer { &raw_arg[1..] } else { raw_arg };

            /*
             * IMPORTANT -
             * it's important to check ALL numeric values in -reverse- size order since,
             * for example, all u8 values could be a u32, but the reverse isn't true!
             * This means that the -smallest- numeric type that can hold the value
             * will be used, by default.
             */

            // Could the argument be an expression?
            if let Some((arg, hint)) = AsmParser::try_parse_expression(substring, is_pointer) {
                if !value_found {
                    arguments.push(arg);
                }

                hints.push(hint);
                value_found = true;
            }

            // Could the argument be a register identifier?
            if let Some((arg, hint)) = AsmParser::try_parse_register_id(substring, is_pointer) {
                if !value_found {
                    arguments.push(arg);
                }

                hints.push(hint);
                value_found = true;
            }

            // Could the argument be a u8 immediate.
            if let Ok(val) = AsmParser::try_parse_u8_immediate(substring) {
                if !value_found {
                    arguments.push(Argument::UnsignedInt(val as u128));
                }

                if is_pointer {
                    hints.push(ArgTypeHint::U8Pointer);
                } else {
                    hints.push(ArgTypeHint::U8);
                }

                value_found = true;
            }

            // Could the argument could be a u32 immediate.
            if let Ok(val) = AsmParser::try_parse_u32_immediate(substring) {
                if !value_found {
                    arguments.push(Argument::UnsignedInt(val as u128));
                }

                if is_pointer {
                    hints.push(ArgTypeHint::U32Pointer);
                } else {
                    hints.push(ArgTypeHint::U32);
                }

                value_found = true;
            }

            // Could this be a floating-point value?
            if substring.contains('.') {
                // Was an address prefix used? This is invalid syntax since f32 values can't
                // be used as pointers.
                if is_pointer {
                    panic!("invalid syntax - unable to use a 32-bit floating value as a pointer!");
                }

                // Could the argument could be a f32 immediate?
                if let Some((arg, hint)) = AsmParser::try_parse_f32_immediate(substring, is_pointer)
                {
                    if !value_found {
                        arguments.push(arg);
                    }

                    hints.push(hint);
                    value_found = true;
                }

                // Could the argument could be a f64 immediate?
                if let Some((arg, hint)) = AsmParser::try_parse_f64_immediate(substring, is_pointer)
                {
                    if !value_found {
                        arguments.push(arg);
                    }

                    hints.push(hint);
                    // TODO - Uncomment if further types are added below.
                    //has_value_pushed = true;
                }

                // The argument was expected to be a valid floating-point value,
                // but we failed to parse it as such. We can't go any further.
                if !value_found {
                    panic!("Failed to parse floating-point value.");
                }
            }

            // Are we dealing with a label? These are placeholders that will be replaced with an
            // address at compile time. For now we'll use dummy values and keep track of the
            // used labels for reference later.
            if substring.starts_with(':') {
                assert!(
                    substring.len() > 1,
                    "invalid syntax - a label designator without a name!"
                );

                // Hold the argument index and the label string for later processing.
                self.labeled_instructions
                    .insert(i, (arguments.len(), substring.to_string()));

                // We want to insert a dummy 32-bit address argument for now.
                arguments.push(Argument::UnsignedInt(DUMMY_LABEL_JUMP_ADDRESS));

                // We also want to use an argument hint that will correspond with
                // the value we're substituting. Since we're working with a 32-bit
                // virtual machine the address size will always be a u32 integer.
                hints.push(ArgTypeHint::U32Pointer);
                value_found = true;
            }

            if !value_found {
                panic!("Unable to parse argument - argument = {substring}");
            }

            argument_hints.push(hints);
        }

        // Calculate the multi-Cartesian product of the argument types.
        let arg_permutations = argument_hints
            .into_iter()
            .multi_cartesian_product()
            .collect_vec();

        let mut possible_matches: Vec<&InstructionLookup<'a>> = if !arguments.is_empty() {
            shortlist
                .iter()
                .filter(|sl| arg_permutations.iter().any(|perm| sl.args == *perm))
                .collect()
        } else {
            shortlist.iter().collect()
        };

        // We will want to select the match with the lowest total argument size.
        possible_matches.sort_by_key(|a| a.total_argument_size());

        // Did we fail to find a match?
        // This can happen because a shortname isn't valid, or because the number or type
        // of arguments don't match.
        assert!(
            !possible_matches.is_empty(),
            "unable to find an instruction that matches the name and provided arguments."
        );

        // Finally, the final match will be whatever entry has been sorted at the top
        // of our vector. The unwrap is safe since we know there is at least one.
        let final_option = possible_matches.first().unwrap();

        // Build our instruction and push it to the list.
        AsmParser::try_build_instruction(final_option.opcode, &arguments)
    }

    /// Try to build an [`Instruction`] from an [`OpCode`] and a set of arguments.
    ///
    /// # Arguments
    ///
    /// * `opcode` - The [`OpCode`] for the instruction.
    /// * `args` - The arguments to be used to build the instruction.
    fn try_build_instruction(opcode: OpCode, args: &[Argument]) -> Instruction {
        use Argument::*;
        use Instruction as I;
        use OpCode as O;

        // This will only ever be called internally and since we have confirmed that the arguments
        // match those that would be needed for the instruction associated with the opcode, it's safe
        // to make some assumptions regarding the sanity of the data.
        match opcode {
            O::Nop => I::Nop,

            /******** [Arithmetic Instructions] ********/
            O::AddU32ImmU32Reg => I::AddU32ImmU32Reg(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u32),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::AddU32RegU32Reg => I::AddU32RegU32Reg(
                get_inner_arg!(args[0], RegisterU32),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::SubU32ImmU32Reg => I::SubU32ImmU32Reg(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u32),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::SubU32RegU32Reg => I::SubU32RegU32Reg(
                get_inner_arg!(args[0], RegisterU32),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::MulU32Imm => I::MulU32Imm(get_inner_arg_and_cast!(args[0], UnsignedInt, u32)),
            O::MulU32Reg => I::MulU32Reg(get_inner_arg!(args[0], RegisterU32)),
            O::DivU32Imm => I::DivU32Imm(get_inner_arg_and_cast!(args[0], UnsignedInt, u32)),
            O::DivU32Reg => I::DivU32Reg(get_inner_arg!(args[0], RegisterU32)),
            O::IncU32Reg => I::IncU32Reg(get_inner_arg!(args[0], RegisterU32)),
            O::DecU32Reg => I::DecU32Reg(get_inner_arg!(args[0], RegisterU32)),
            O::AndU32ImmU32Reg => I::AndU32ImmU32Reg(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u32),
                get_inner_arg!(args[1], RegisterU32),
            ),

            /******** [Bit Operation Instructions] ********/
            O::LeftShiftU8ImmU32Reg => I::LeftShiftU8ImmU32Reg(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u8),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::LeftShiftU32RegU32Reg => I::LeftShiftU32RegU32Reg(
                get_inner_arg!(args[0], RegisterU32),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::ArithLeftShiftU8ImmU32Reg => I::ArithLeftShiftU8ImmU32Reg(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u8),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::ArithLeftShiftU32RegU32Reg => I::ArithLeftShiftU32RegU32Reg(
                get_inner_arg!(args[0], RegisterU32),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::RightShiftU8ImmU32Reg => I::RightShiftU8ImmU32Reg(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u8),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::RightShiftU32RegU32Reg => I::RightShiftU32RegU32Reg(
                get_inner_arg!(args[0], RegisterU32),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::ArithRightShiftU8ImmU32Reg => I::ArithRightShiftU8ImmU32Reg(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u8),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::ArithRightShiftU32RegU32Reg => I::ArithRightShiftU32RegU32Reg(
                get_inner_arg!(args[0], RegisterU32),
                get_inner_arg!(args[1], RegisterU32),
            ),

            /******** [Branching Instructions] ********/
            O::CallU32Imm => I::CallU32Imm(get_inner_arg_and_cast!(args[0], UnsignedInt, u32)),
            O::CallU32Reg => I::CallU32Reg(get_inner_arg!(args[0], RegisterU32)),
            O::RetArgsU32 => I::RetArgsU32,
            O::Int => I::Int(get_inner_arg_and_cast!(args[0], UnsignedInt, u8)),
            O::IntRet => I::IntRet,
            O::JumpAbsU32Imm => {
                I::JumpAbsU32Imm(get_inner_arg_and_cast!(args[0], UnsignedInt, u32))
            }
            O::JumpAbsU32Reg => I::JumpAbsU32Reg(get_inner_arg!(args[0], RegisterU32)),

            /******** [Data Instructions] ********/
            O::SwapU32RegU32Reg => I::SwapU32RegU32Reg(
                get_inner_arg!(args[0], RegisterU32),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::MovU32ImmU32Reg => I::MovU32ImmU32Reg(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u32),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::MovU32RegU32Reg => I::MovU32RegU32Reg(
                get_inner_arg!(args[0], RegisterU32),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::MovU32ImmMemSimple => I::MovU32ImmMemSimple(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u32),
                get_inner_arg_and_cast!(args[1], UnsignedInt, u32),
            ),
            O::MovU32RegMemSimple => I::MovU32RegMemSimple(
                get_inner_arg!(args[0], RegisterU32),
                get_inner_arg_and_cast!(args[1], UnsignedInt, u32),
            ),
            O::MovMemU32RegSimple => I::MovMemU32RegSimple(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u32),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::MovU32RegPtrU32RegSimple => I::MovU32RegPtrU32RegSimple(
                get_inner_arg!(args[0], RegisterU32),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::MovU32ImmMemExpr => I::MovU32ImmMemExpr(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u32),
                get_inner_expr_arg!(args[1]),
            ),
            O::MovMemExprU32Reg => I::MovMemExprU32Reg(
                get_inner_expr_arg!(args[0]),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::MovU32RegMemExpr => I::MovU32RegMemExpr(
                get_inner_arg!(args[0], RegisterU32),
                get_inner_expr_arg!(args[1]),
            ),
            O::ByteSwapU32 => I::ByteSwapU32(get_inner_arg!(args[0], RegisterU32)),
            O::ZeroHighBitsByIndexU32Reg => I::ZeroHighBitsByIndexU32Reg(
                get_inner_arg!(args[0], RegisterU32),
                get_inner_arg!(args[1], RegisterU32),
                get_inner_arg!(args[2], RegisterU32),
            ),
            O::ZeroHighBitsByIndexU32RegU32Imm => I::ZeroHighBitsByIndexU32RegU32Imm(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u32),
                get_inner_arg!(args[1], RegisterU32),
                get_inner_arg!(args[2], RegisterU32),
            ),
            O::PushF32Imm => I::PushF32Imm(get_inner_arg_and_cast!(args[0], Float, f32)),
            O::PushU32Imm => I::PushU32Imm(get_inner_arg_and_cast!(args[0], UnsignedInt, u32)),
            O::PushU32Reg => I::PushU32Reg(get_inner_arg!(args[0], RegisterU32)),
            O::PopF32ToF32Reg => I::PopF32ToF32Reg(get_inner_arg!(args[0], RegisterF32)),
            O::PopU32ToU32Reg => I::PopU32ToU32Reg(get_inner_arg!(args[0], RegisterU32)),

            /******** [IO Instructions] ********/
            O::OutF32Imm => I::OutF32Imm(
                get_inner_arg_and_cast!(args[0], Float, f32),
                get_inner_arg_and_cast!(args[1], UnsignedInt, u8),
            ),
            O::OutU32Imm => I::OutU32Imm(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u32),
                get_inner_arg_and_cast!(args[1], UnsignedInt, u8),
            ),
            O::OutU32Reg => I::OutU32Reg(
                get_inner_arg!(args[0], RegisterU32),
                get_inner_arg_and_cast!(args[1], UnsignedInt, u8),
            ),
            O::OutU8Imm => I::OutU8Imm(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u8),
                get_inner_arg_and_cast!(args[1], UnsignedInt, u8),
            ),
            O::InU8Reg => I::InU8Reg(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u8),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::InU8Mem => I::InU8Mem(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u8),
                get_inner_arg_and_cast!(args[1], UnsignedInt, u32),
            ),
            O::InU32Reg => I::InU32Reg(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u8),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::InU32Mem => I::InU32Mem(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u8),
                get_inner_arg_and_cast!(args[1], UnsignedInt, u32),
            ),
            O::InF32Reg => I::InF32Reg(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u8),
                get_inner_arg!(args[1], RegisterF32),
            ),
            O::InF32Mem => I::InF32Mem(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u8),
                get_inner_arg_and_cast!(args[1], UnsignedInt, u32),
            ),

            /******** [Logic Instructions] ********/
            O::BitTestU32Reg => I::BitTestU32Reg(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u8),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::BitTestU32Mem => I::BitTestU32Mem(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u8),
                get_inner_arg_and_cast!(args[1], UnsignedInt, u32),
            ),
            O::BitTestResetU32Reg => I::BitTestResetU32Reg(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u8),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::BitTestResetU32Mem => I::BitTestResetU32Mem(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u8),
                get_inner_arg_and_cast!(args[1], UnsignedInt, u32),
            ),
            O::BitTestSetU32Reg => I::BitTestSetU32Reg(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u8),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::BitTestSetU32Mem => I::BitTestSetU32Mem(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u8),
                get_inner_arg_and_cast!(args[1], UnsignedInt, u32),
            ),
            O::BitScanReverseU32RegU32Reg => I::BitScanReverseU32RegU32Reg(
                get_inner_arg!(args[0], RegisterU32),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::BitScanReverseU32MemU32Reg => I::BitScanReverseU32MemU32Reg(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u32),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::BitScanReverseU32RegMemU32 => I::BitScanReverseU32RegMemU32(
                get_inner_arg!(args[0], RegisterU32),
                get_inner_arg_and_cast!(args[1], UnsignedInt, u32),
            ),
            O::BitScanReverseU32MemU32Mem => I::BitScanReverseU32MemU32Mem(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u32),
                get_inner_arg_and_cast!(args[1], UnsignedInt, u32),
            ),
            O::BitScanForwardU32RegU32Reg => I::BitScanForwardU32RegU32Reg(
                get_inner_arg!(args[0], RegisterU32),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::BitScanForwardU32MemU32Reg => I::BitScanForwardU32MemU32Reg(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u32),
                get_inner_arg!(args[1], RegisterU32),
            ),
            O::BitScanForwardU32RegMemU32 => I::BitScanForwardU32RegMemU32(
                get_inner_arg!(args[0], RegisterU32),
                get_inner_arg_and_cast!(args[1], UnsignedInt, u32),
            ),
            O::BitScanForwardU32MemU32Mem => I::BitScanForwardU32MemU32Mem(
                get_inner_arg_and_cast!(args[0], UnsignedInt, u32),
                get_inner_arg_and_cast!(args[1], UnsignedInt, u32),
            ),

            /******** [Special Instructions] ********/
            O::MaskInterrupt => I::MaskInterrupt(get_inner_arg_and_cast!(args[0], UnsignedInt, u8)),
            O::UnmaskInterrupt => {
                I::UnmaskInterrupt(get_inner_arg_and_cast!(args[0], UnsignedInt, u8))
            }
            O::LoadIVTAddrU32Imm => {
                I::LoadIVTAddrU32Imm(get_inner_arg_and_cast!(args[0], UnsignedInt, u32))
            }
            O::MachineReturn => I::MachineReturn,
            O::Halt => I::Halt,

            /******** [Reserved Instructions] ********/
            O::Reserved1 => unreachable!(),
            O::Reserved2 => unreachable!(),
            O::Reserved3 => unreachable!(),
            O::Reserved4 => unreachable!(),
            O::Reserved5 => unreachable!(),
            O::Reserved6 => unreachable!(),
            O::Reserved7 => unreachable!(),
            O::Reserved8 => unreachable!(),
            O::Reserved9 => unreachable!(),

            /******** [Pseudo Instructions] ********/
            O::Label => unreachable!(),
            O::Unknown => unreachable!(),
        }
    }

    /// Try to parse an expression.
    ///
    /// # Arguments
    ///
    /// * `string` - The input string.
    /// * `is_pointer` - Is this argument a pointer?
    #[inline]
    fn try_parse_expression(string: &str, is_pointer: bool) -> Option<(Argument, ArgTypeHint)> {
        // Expressions must start with an open square bracket and end with a close square bracket.
        let first_char = string.chars().nth(0).unwrap();
        let last_char = string.chars().last().unwrap();
        if first_char != '[' || last_char != ']' {
            return None;
        }

        // Skip over the brackets.
        let expr_substring = &string[1..string.len() - 1];

        // An expression should be composed of two or three components separated by an operator.
        // Each component may be either a register ID or a u8 value.
        let mut expr_arguments = vec![];

        let mut segment_end = false;
        let mut start_pos = 0;
        let mut end_pos = 0;

        let mut chars = expr_substring.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                ' ' | '+' | '-' | '*' => {
                    segment_end = true;
                }
                _ => {}
            }

            // We always want to be sure to catch the last character.
            if chars.peek().is_none() {
                segment_end = true;
                end_pos += 1;
            }

            if segment_end {
                let argument = &expr_substring[start_pos..end_pos];

                // If we have a non-empty string then we can add it to our processing list.
                if !argument.is_empty() {
                    if let Some((arg, _)) = AsmParser::try_parse_register_id(argument, false) {
                        // Do we have a register identifier.
                        let value = get_inner_arg!(arg, Argument::RegisterU32);
                        expr_arguments.push(Register(value));
                    } else if let Some((arg, _)) = AsmParser::try_parse_u8(argument, false) {
                        // Do we have a u8 immediate?
                        let value = get_inner_arg_and_cast!(arg, Argument::UnsignedInt, u8);
                        expr_arguments.push(Immediate(value));
                    } else {
                        // Something other than our permitted components.
                        break;
                    }
                }

                // Do we need to add an operator?
                match c {
                    '+' => {
                        expr_arguments.push(Operator(Add));
                    }
                    '-' => {
                        expr_arguments.push(Operator(Subtract));
                    }
                    '*' => {
                        expr_arguments.push(Operator(Multiply));
                    }
                    _ => {}
                }

                // Skip over the current character to the next one.
                start_pos = end_pos + 1;

                segment_end = false;
            }

            end_pos += 1;
        }

        // Is the expression argument list valid?
        if let Ok(expr) = Expression::try_from(&expr_arguments[..]) {
            if is_pointer {
                Some((Argument::Expression(expr), ArgTypeHint::ExpressionPointer))
            } else {
                Some((Argument::Expression(expr), ArgTypeHint::Expression))
            }
        } else {
            panic!("Invalid expression syntax - {expr_substring}");
        }
    }

    /// Try to parse an argument string as a u8.
    ///
    /// # Arguments
    ///
    /// * `string` - The input string.
    /// * `is_pointer` - Is this argument a pointer?
    #[inline]
    fn try_parse_u8(string: &str, is_pointer: bool) -> Option<(Argument, ArgTypeHint)> {
        match AsmParser::try_parse_u8_immediate(string) {
            Ok(val) => {
                if is_pointer {
                    Some((Argument::UnsignedInt(val as u128), ArgTypeHint::U8Pointer))
                } else {
                    Some((Argument::UnsignedInt(val as u128), ArgTypeHint::U8))
                }
            }
            Err(_) => None,
        }
    }

    /// Try to parse a a register ID.
    ///
    /// # Arguments
    ///
    /// * `string` - The input string.
    /// * `is_pointer` - Is this argument a pointer?
    #[inline]
    fn try_parse_register_id(string: &str, is_pointer: bool) -> Option<(Argument, ArgTypeHint)> {
        match RegisterId::from_str(string) {
            Ok(id) => {
                if U32_REGISTERS.contains(&id) {
                    if is_pointer {
                        Some((Argument::RegisterU32(id), ArgTypeHint::RegisterU32Pointer))
                    } else {
                        Some((Argument::RegisterU32(id), ArgTypeHint::RegisterU32))
                    }
                } else if F32_REGISTERS.contains(&id) {
                    if is_pointer {
                        panic!("It's not possible to use a f32 register as a pointer!")
                    } else {
                        Some((Argument::RegisterF32(id), ArgTypeHint::RegisterF32))
                    }
                } else {
                    panic!("Unclassified register identifier = {id}");
                }
            }
            Err(_) => None,
        }
    }

    fn try_parse_f32_immediate(string: &str, is_pointer: bool) -> Option<(Argument, ArgTypeHint)> {
        if !string.contains('.') {
            return None;
        }

        // Was an address prefix used? This is invalid syntax since f32 values can't
        // be used as pointers.
        if is_pointer {
            panic!("invalid syntax - unable to use a 32-bit floating value as a pointer!");
        }

        if let Ok(val) = f32::from_str(string) {
            // The argument could be a f32 immediate.
            Some((Argument::Float(val as f64), ArgTypeHint::F32))
        } else {
            None
        }
    }

    fn try_parse_f64_immediate(string: &str, is_pointer: bool) -> Option<(Argument, ArgTypeHint)> {
        if !string.contains('.') {
            return None;
        }

        // Was an address prefix used? This is invalid syntax since f32 values can't
        // be used as pointers.
        if is_pointer {
            panic!("invalid syntax - unable to use a 64-bit floating value as a pointer!");
        }

        if let Ok(val) = f64::from_str(string) {
            // The argument could be a f64 immediate.
            Some((Argument::Float(val), ArgTypeHint::F64))
        } else {
            None
        }
    }

    /// Try to parse a u8 immediate value.
    ///
    /// # Arguments
    ///
    /// * `string` - A string that may contain the value.
    fn try_parse_u8_immediate(string: &str) -> Result<u8, ParseIntError> {
        // Check for immediate values in specific bases.
        // I can't imagine these would be used much... but why not?
        if string.starts_with("0b") {
            // Binary.
            let stripped = string.strip_prefix("0b").expect("");
            u8::from_str_radix(stripped, 2)
        } else if string.starts_with("0o") {
            // Octal.
            let stripped = string.strip_prefix("0o").expect("");
            u8::from_str_radix(stripped, 8)
        } else if string.starts_with("0x") {
            // Hex.
            let stripped = string.strip_prefix("0x").expect("");
            u8::from_str_radix(stripped, 16)
        } else {
            string.parse::<u8>()
        }
    }

    /// Try to parse a u32 immediate value.
    ///
    /// # Arguments
    ///
    /// * `string` - A string that may contain the value.
    fn try_parse_u32_immediate(string: &str) -> Result<u32, ParseIntError> {
        // Check for immediate values in specific bases.
        // I can't imagine these would be used much... but why not?
        if string.starts_with("0b") {
            // Binary.
            let stripped = string.strip_prefix("0b").expect("");
            u32::from_str_radix(stripped, 2)
        } else if string.starts_with("0o") {
            // Octal.
            let stripped = string.strip_prefix("0o").expect("");
            u32::from_str_radix(stripped, 8)
        } else if string.starts_with("0x") {
            // Hex.
            let stripped = string.strip_prefix("0x").expect("");
            u32::from_str_radix(stripped, 16)
        } else {
            string.parse::<u32>()
        }
    }

    /// Try to parse an instruction line.
    ///
    /// # Arguments
    ///
    /// * `line` - A string slice giving the line to be parsed.
    pub fn parse_instruction_line(line: &str) -> Vec<String> {
        let mut segments = Vec::with_capacity(5);

        let mut skip_to_end = false;
        let mut segment_end = false;
        let mut start_pos = 0;
        let mut end_pos = 0;

        let chars = line.chars().collect_vec();
        let len = chars.len();

        for (i, c) in chars.iter().enumerate() {
            // What type of character are we dealing with?
            match c {
                ' ' | ',' => {
                    segment_end = true;
                }
                ';' => {
                    skip_to_end = true;
                    segment_end = true;
                }
                _ => {}
            }

            // We always want to be sure to catch the last character.
            if i == len - 1 {
                segment_end = true;
                end_pos += 1;
            }

            if segment_end {
                let string = &line[start_pos..end_pos];

                // If we have a non-empty string then we can add it to our processing list.
                if !string.is_empty() {
                    segments.push(string.to_string());
                }

                // Skip over the current character to the next one.
                start_pos = end_pos + 1;

                // Start a new segment.
                segment_end = false;
            }

            // If we have encountered a comment then we want to skip everything on the rest of this line.
            if skip_to_end {
                break;
            }

            end_pos += 1;
        }

        segments
    }
}

impl<'a> Default for AsmParser<'a> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests_asm_parsing {
    use std::panic;

    use strum::IntoEnumIterator;

    use redox_core::{
        ins::{instruction::Instruction, op_codes::OpCode},
        reg::registers::RegisterId,
    };

    use crate::asm_parser::AsmParser;

    use super::DUMMY_LABEL_JUMP_ADDRESS;

    #[derive(Clone)]
    struct ParserTest {
        /// The input string to be tested.
        pub input: String,
        /// A vector of [`Instruction`]s that should result from the parsing.
        pub expected_instructions: Vec<Instruction>,
        /// A boolean indicating whether the test should panic or not.
        pub should_panic: bool,
        /// A string slice that provides the message to be displayed if the test fails.
        pub fail_message: String,
    }

    impl ParserTest {
        /// Create a new [`ParserTest`] instance.
        ///
        /// # Arguments
        ///
        /// * `input` - The input string to be tested.
        /// * `expected_instructions` - A slice of [`Instruction`]s that should result from the parsing.
        /// * `should_panic` - A boolean indicating whether the test should panic or not.
        /// * `fail_message` - A string slice that provides the message to be displayed if the test fails.
        ///
        /// # Note
        ///
        /// If the results need to check the user segment memory contents then the VM will automatically be
        /// created with a memory segment of the correct size. It doesn't need to be specified manually.
        fn new(
            input: &str,
            expected_instructions: &[Instruction],
            should_panic: bool,
            fail_message: &str,
        ) -> Self {
            Self {
                input: input.to_string(),
                expected_instructions: expected_instructions.to_vec(),
                should_panic,
                fail_message: fail_message.to_string(),
            }
        }

        /// Run this specific test entry.
        ///
        /// # Arguments
        ///
        /// * `id` - The ID of this test.
        pub fn run_test(&self, id: usize) {
            // Attempt to execute the code.
            let result = panic::catch_unwind(|| {
                let mut parser = AsmParser::new();
                parser.parse(&self.input);

                parser
            });

            // Confirm whether the test panicked, and whether that panic was expected or not.
            let did_panic = result.is_err();
            assert_eq!(
                did_panic,
                self.should_panic,
                "{}",
                self.fail_message(id, did_panic)
            );

            // We don't have a viable list to interrogate here.
            if !did_panic {
                assert_eq!(
                    result.unwrap().parsed_instructions,
                    self.expected_instructions
                );
            }
        }

        /// Generate a fail message for this test instance.
        ///
        /// # Arguments
        ///
        /// * `id` - The ID of this test.
        /// * `did_panic` - Did the test panic?
        pub fn fail_message(&self, id: usize, did_panic: bool) -> String {
            format!(
                "Test {id} Failed - Should Panic? {}, Panicked? {did_panic}. Message = {}",
                self.should_panic, self.fail_message
            )
        }
    }

    struct ParserTests {
        tests: Vec<ParserTest>,
    }

    impl ParserTests {
        pub fn new(tests: &[ParserTest]) -> Self {
            Self {
                tests: tests.to_vec(),
            }
        }

        /// Run each unit test in the specified sequence.
        pub fn run_all(&self) {
            for (id, test) in self.tests.iter().enumerate() {
                test.run_test(id);
            }
        }
    }

    #[test]
    fn code_parser_labels() {
        let tests = [
            ParserTest::new(
                "nop\r\n:LABEL_1",
                &[
                    Instruction::Nop,
                    Instruction::Label(String::from(":LABEL_1")),
                ],
                false,
                "failed to correctly parse label instruction.",
            ),
            ParserTest::new(
                "call :LABEL_1",
                &[Instruction::CallU32Imm(DUMMY_LABEL_JUMP_ADDRESS as u32)],
                false,
                "failed to correctly parse instruction label.",
            ),
            ParserTest::new(
                ":LABEL_1 EVERYTHING HERE SHOULD BE IGNORED",
                &[Instruction::Label(String::from(":LABEL_1"))],
                false,
                "failed to correctly parse label instruction.",
            ),
            ParserTest::new(":", &[], true, "succeeded in parsing an empty label."),
            ParserTest::new("call :", &[], true, "succeeded in parsing an empty label."),
        ];

        ParserTests::new(&tests).run_all();
    }

    #[test]
    fn code_parser_comments() {
        let tests = [
            ParserTest::new(
                "call &0xdeadbeef ; this comment should be ignored.",
                &[Instruction::CallU32Imm(0xdeadbeef)],
                false,
                "failed to correctly parse instruction and comment.",
            ),
            ParserTest::new(
                "nop\r\n;this should be ignored\r\nnop",
                &[Instruction::Nop, Instruction::Nop],
                false,
                "failed to correctly parse instruction and comment.",
            ),
            ParserTest::new(
                "nop\r\n;nop\r\nnop",
                &[Instruction::Nop, Instruction::Nop],
                false,
                "failed to correctly parse instruction and comment.",
            ),
        ];

        ParserTests::new(&tests).run_all();
    }

    /// Instruction parsing tests - all invalid.
    #[test]
    fn code_parser_tests_invalid() {
        let tests = [
            // This is invalid because the argument to call must be an integer register or integer immediate pointer.
            ParserTest::new(
                "call 0xdeadbeef",
                &[],
                true,
                "succeeded in parsing instruction with invalid arguments.",
            ),
            // This is invalid because the argument to call must be an integer register or integer immediate pointer.
            ParserTest::new(
                "call ER1",
                &[],
                true,
                "succeeded in parsing instruction with invalid arguments.",
            ),
            // This is invalid because the argument to call must be an integer register or integer immediate pointer.
            ParserTest::new(
                "call 0.1234",
                &[],
                true,
                "succeeded in parsing instruction with invalid arguments.",
            ),
            // This is invalid because the argument to call must be an integer register or integer immediate pointer.
            ParserTest::new(
                "call &FP1",
                &[],
                true,
                "succeeded in parsing instruction with invalid arguments.",
            ),
            // This is invalid because the register ID doesn't exist.
            ParserTest::new(
                "call &AAA",
                &[],
                true,
                "succeeded in parsing instruction with invalid arguments.",
            ),
            // This is invalid because we have an open square bracket (indicating an expression) but no
            // closing one. This is invalid syntax.
            ParserTest::new(
                "mov &[ER1*2, ER1",
                &[],
                true,
                "succeeded in parsing instruction with invalid arguments.",
            ),
            // This is invalid because we have a closing square bracket (indicating an expression) but no
            // opening one. This is invalid syntax.
            ParserTest::new(
                "mov &ER1*2], ER1",
                &[],
                true,
                "succeeded in parsing instruction with invalid arguments.",
            ),
            // This is invalid because the expression containing an invalid register.
            ParserTest::new(
                "mov &[ERQ*2], ER1",
                &[],
                true,
                "succeeded in parsing instruction with invalid arguments.",
            ),
            // This is invalid because the value is larger than supported by a u8 value.
            // In an expression we may only use u8 values.
            ParserTest::new(
                "mov &[ER1*999], ER1",
                &[],
                true,
                "succeeded in parsing instruction with invalid arguments.",
            ),
            // This is invalid because we can't use floats in expressions.
            ParserTest::new(
                "mov &[ER1*1.0], ER1",
                &[],
                true,
                "succeeded in parsing instruction with invalid arguments.",
            ),
            // This is invalid because we have an operator with nothing following it.
            ParserTest::new(
                "mov &[ER1*], ER1",
                &[],
                true,
                "succeeded in parsing instruction with invalid arguments.",
            ),
            // This is invalid because we have an operator with nothing preceding it.
            ParserTest::new(
                "mov &[*ER1], ER1",
                &[],
                true,
                "succeeded in parsing instruction with invalid arguments.",
            ),
            // This is invalid because we have an operator with nothing following it.
            ParserTest::new(
                "mov &[ER1*ER2*], ER1",
                &[],
                true,
                "succeeded in parsing instruction with invalid arguments.",
            ),
            // This is invalid because we have too many arguments within the expression.
            // We may, at most, have three values.
            ParserTest::new(
                "mov &[ER1*ER2*ER3*ER4], ER1",
                &[],
                true,
                "succeeded in parsing instruction with invalid arguments.",
            ),
            // This is invalid because a single period is not a valid floating-point value.
            ParserTest::new(
                "mov ., ER1",
                &[],
                true,
                "succeeded in parsing instruction with invalid arguments.",
            ),
            // This is invalid because a number followed by single period with no following digits
            // is not a valid floating-point value.
            ParserTest::new(
                "mov 1., ER1",
                &[],
                true,
                "succeeded in parsing instruction with invalid arguments.",
            ),
        ];

        ParserTests::new(&tests).run_all();
    }

    /// Single instruction parsing tests - with numbers in different bases.
    #[test]
    fn code_parser_tests_numeric_bases() {
        let tests = [
            ParserTest::new(
                "push 0b1111111111",
                &[Instruction::PushU32Imm(0b1111111111)],
                false,
                "failed to parse instruction with u32 argument - binary edition.",
            ),
            ParserTest::new(
                "push 0o12345",
                &[Instruction::PushU32Imm(0o12345)],
                false,
                "failed to parse instruction with u32 argument - octal edition.",
            ),
            ParserTest::new(
                "push 1234",
                &[Instruction::PushU32Imm(1234)],
                false,
                "failed to parse instruction with u32 argument - decimal edition.",
            ),
            ParserTest::new(
                "push 0x1234",
                &[Instruction::PushU32Imm(0x1234)],
                false,
                "failed to parse instruction with u32 argument - hex edition.",
            ),
        ];

        ParserTests::new(&tests).run_all();
    }

    #[test]
    fn parse_instruction_round_trip() {
        use redox_core::{
            ins::expressions::{Expression, ExpressionArgs::*, ExpressionOperator::*},
            reg::registers::RegisterId::*,
        };

        let expr = Expression::try_from(&[Immediate(0x8), Operator(Add), Immediate(0x8)][..])
            .expect("")
            .pack();

        let mut instructions = Vec::new();

        use redox_core::ins::instruction::Instruction as I;
        use redox_core::ins::op_codes::OpCode as O;

        // This might seem a little long-winded, but it's done this way to ensure
        // that each time a new instruction is added that a corresponding entry
        // is added here.
        for opcode in OpCode::iter() {
            let ins = match opcode {
                O::Nop => I::Nop,
                O::AddU32ImmU32Reg => I::AddU32ImmU32Reg(0x123, ER2),
                O::AddU32RegU32Reg => I::AddU32RegU32Reg(ER2, ER3),
                O::SubU32ImmU32Reg => I::SubU32ImmU32Reg(0x123, ER2),
                O::SubU32RegU32Reg => I::SubU32RegU32Reg(ER2, ER3),
                O::MulU32Imm => I::MulU32Imm(0x123),
                O::MulU32Reg => I::MulU32Reg(ER2),
                O::DivU32Imm => I::DivU32Imm(0x123),
                O::DivU32Reg => I::DivU32Reg(ER2),
                O::IncU32Reg => I::IncU32Reg(ER2),
                O::DecU32Reg => I::DecU32Reg(ER2),
                O::AndU32ImmU32Reg => I::AndU32ImmU32Reg(0x123, ER2),
                O::LeftShiftU8ImmU32Reg => I::LeftShiftU8ImmU32Reg(31, ER2),
                O::LeftShiftU32RegU32Reg => I::LeftShiftU32RegU32Reg(ER2, ER3),
                O::ArithLeftShiftU8ImmU32Reg => I::ArithLeftShiftU8ImmU32Reg(31, ER2),
                O::ArithLeftShiftU32RegU32Reg => I::ArithLeftShiftU32RegU32Reg(ER2, ER3),
                O::RightShiftU8ImmU32Reg => I::RightShiftU8ImmU32Reg(31, ER2),
                O::RightShiftU32RegU32Reg => I::RightShiftU32RegU32Reg(ER2, ER3),
                O::ArithRightShiftU8ImmU32Reg => I::ArithRightShiftU8ImmU32Reg(31, ER2),
                O::ArithRightShiftU32RegU32Reg => I::ArithRightShiftU32RegU32Reg(ER2, ER3),
                O::CallU32Imm => I::CallU32Imm(0xdeafbeef),
                O::CallU32Reg => I::CallU32Reg(RegisterId::ER2),
                O::RetArgsU32 => I::RetArgsU32,
                O::Int => I::Int(0xff),
                O::IntRet => I::IntRet,
                O::JumpAbsU32Imm => I::JumpAbsU32Imm(0xdeadbeef),
                O::JumpAbsU32Reg => I::JumpAbsU32Reg(ER1),
                O::SwapU32RegU32Reg => I::SwapU32RegU32Reg(ER2, ER3),
                O::MovU32ImmU32Reg => I::MovU32ImmU32Reg(0x123, ER2),
                O::MovU32RegU32Reg => I::MovU32RegU32Reg(ER2, ER3),
                O::MovU32ImmMemSimple => I::MovU32ImmMemSimple(0x123, 0x321),
                O::MovU32RegMemSimple => I::MovU32RegMemSimple(ER2, 0x123),
                O::MovMemU32RegSimple => I::MovMemU32RegSimple(0x123, ER2),
                O::MovU32RegPtrU32RegSimple => I::MovU32RegPtrU32RegSimple(ER2, ER3),
                O::MovU32ImmMemExpr => I::MovU32ImmMemExpr(0x321, expr),
                O::MovMemExprU32Reg => I::MovMemExprU32Reg(expr, ER2),
                O::MovU32RegMemExpr => I::MovU32RegMemExpr(ER2, expr),
                O::ByteSwapU32 => I::ByteSwapU32(ER2),
                O::ZeroHighBitsByIndexU32Reg => I::ZeroHighBitsByIndexU32Reg(ER2, ER3, ER4),
                O::ZeroHighBitsByIndexU32RegU32Imm => {
                    I::ZeroHighBitsByIndexU32RegU32Imm(0x123, ER2, ER3)
                }
                O::PushU32Imm => I::PushU32Imm(0x123),
                O::PushF32Imm => I::PushF32Imm(0.1),
                O::PushU32Reg => I::PushU32Reg(ER2),
                O::PopF32ToF32Reg => I::PopF32ToF32Reg(FR2),
                O::PopU32ToU32Reg => I::PopU32ToU32Reg(ER2),
                O::OutF32Imm => I::OutF32Imm(1.0, 0xab),
                O::OutU32Imm => I::OutU32Imm(0xdeadbeef, 0xab),
                O::OutU32Reg => I::OutU32Reg(ER2, 0xab),
                O::OutU8Imm => I::OutU8Imm(0xba, 0xab),
                O::InU8Reg => I::InU8Reg(0xab, ER2),
                O::InU8Mem => I::InU8Mem(0xab, 0xdeadbeef),
                O::InU32Reg => I::InU32Reg(0xab, ER2),
                O::InU32Mem => I::InU32Mem(0xab, 0xdeadbeef),
                O::InF32Reg => I::InF32Reg(0xab, FR2),
                O::InF32Mem => I::InF32Mem(0xab, 0xdeadbeef),
                O::BitTestU32Reg => I::BitTestU32Reg(0x40, ER2),
                O::BitTestU32Mem => I::BitTestU32Mem(0x40, 0x123),
                O::BitTestResetU32Reg => I::BitTestResetU32Reg(0x40, ER2),
                O::BitTestResetU32Mem => I::BitTestResetU32Mem(0x40, 0x123),
                O::BitTestSetU32Reg => I::BitTestSetU32Reg(0x40, ER2),
                O::BitTestSetU32Mem => I::BitTestSetU32Mem(0x40, 0x123),
                O::BitScanReverseU32RegU32Reg => I::BitScanReverseU32RegU32Reg(ER2, ER3),
                O::BitScanReverseU32MemU32Reg => I::BitScanReverseU32MemU32Reg(0x123, ER2),
                O::BitScanReverseU32RegMemU32 => I::BitScanReverseU32RegMemU32(ER2, 0x123),
                O::BitScanReverseU32MemU32Mem => I::BitScanReverseU32MemU32Mem(0x123, 0x321),
                O::BitScanForwardU32RegU32Reg => I::BitScanForwardU32RegU32Reg(ER2, ER3),
                O::BitScanForwardU32MemU32Reg => I::BitScanForwardU32MemU32Reg(0x123, ER2),
                O::BitScanForwardU32RegMemU32 => I::BitScanForwardU32RegMemU32(ER2, 0x123),
                O::BitScanForwardU32MemU32Mem => I::BitScanForwardU32MemU32Mem(0x123, 0x321),
                O::MaskInterrupt => I::MaskInterrupt(0xff),
                O::UnmaskInterrupt => I::UnmaskInterrupt(0xff),
                O::LoadIVTAddrU32Imm => I::LoadIVTAddrU32Imm(0xdeadbeef),
                O::MachineReturn => I::MachineReturn,
                O::Halt => I::Halt,

                // We don't want to test constructing these instructions.
                O::Reserved1
                | O::Reserved2
                | O::Reserved3
                | O::Reserved4
                | O::Reserved5
                | O::Reserved6
                | O::Reserved7
                | O::Reserved8
                | O::Reserved9
                | O::Label
                | O::Unknown => continue,
            };

            instructions.push(ins);
        }

        let mut failed = false;
        for (i, ins) in instructions.iter().enumerate() {
            let ins_str = ins.to_string();
            let parser = panic::catch_unwind(|| {
                let mut asm_parser = AsmParser::new();
                asm_parser.parse(&ins_str);
                asm_parser
            });
            if let Ok(p) = parser {
                let result = p.parsed_instructions.first().expect("");
                if result != ins {
                    println!("Test {i} failed! Original = {ins:?}, actual = {result:?}.");
                    failed = true;
                }
            } else {
                println!("Test {i} failed! No matching instruction was found. Input = {ins_str}");
                failed = true;
            }
        }

        assert!(!failed, "one or more tests failed to correctly round-trip");
    }
}
