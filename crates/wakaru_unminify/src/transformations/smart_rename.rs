use std::collections::{HashMap, HashSet};

use oxc_ast::{
    ast::{
        Argument, BindingIdentifier, BindingPattern, BindingProperty, CallExpression, Expression,
        FormalParameters, IdentifierReference, PropertyKey, VariableDeclarator,
    },
    AstBuilder,
};
use oxc_ast_visit::{walk_mut, VisitMut};
use oxc_semantic::{ScopeId, Scoping, SemanticBuilder, SymbolId};
use wakaru_core::diagnostics::Result;
use wakaru_core::source::ParsedSourceFile;

const MINIFIED_IDENTIFIER_THRESHOLD: usize = 2;

pub fn transform_ast(source: &mut ParsedSourceFile) -> Result<()> {
    let scoping = SemanticBuilder::new()
        .build(&source.program)
        .semantic
        .into_scoping();

    let mut collector = DestructuringRenameCollector {
        ast: AstBuilder::new(source.allocator),
        scoping: &scoping,
        generated_names: HashMap::new(),
        renames: HashMap::new(),
    };
    collector.visit_program(&mut source.program);

    let mut renamer = SymbolRenamer {
        ast: AstBuilder::new(source.allocator),
        scoping: &scoping,
        renames: collector.renames,
    };
    renamer.visit_program(&mut source.program);

    Ok(())
}

struct DestructuringRenameCollector<'a, 's> {
    ast: AstBuilder<'a>,
    scoping: &'s Scoping,
    generated_names: HashMap<ScopeId, HashSet<String>>,
    renames: HashMap<SymbolId, String>,
}

impl<'a> VisitMut<'a> for DestructuringRenameCollector<'a, '_> {
    fn visit_variable_declarator(&mut self, declarator: &mut VariableDeclarator<'a>) {
        self.rename_create_context(declarator);
        self.rename_use_ref(declarator);
        self.rename_use_state_setter(declarator);
        self.rename_use_reducer_pair(declarator);
        self.rename_forward_ref_params(declarator);
        walk_mut::walk_variable_declarator(self, declarator);
    }

    fn visit_binding_property(&mut self, property: &mut BindingProperty<'a>) {
        self.rename_property(property);
        walk_mut::walk_binding_property(self, property);
    }
}

impl<'a> DestructuringRenameCollector<'a, '_> {
    fn rename_create_context(&mut self, declarator: &mut VariableDeclarator<'a>) {
        let Some(call) = call_expression_init(declarator) else {
            return;
        };
        if !is_react_call(call, "createContext") || call.arguments.len() > 1 {
            return;
        }

        let BindingPattern::BindingIdentifier(identifier) = &mut declarator.id else {
            return;
        };
        if identifier.name.len() > MINIFIED_IDENTIFIER_THRESHOLD {
            return;
        }

        let old_name = identifier.name.as_str().to_string();
        self.rename_binding_to_generated(identifier, &format!("{}Context", pascal_case(&old_name)));
    }

    fn rename_use_ref(&mut self, declarator: &mut VariableDeclarator<'a>) {
        let Some(call) = call_expression_init(declarator) else {
            return;
        };
        if !is_react_call(call, "useRef") || call.arguments.len() > 1 {
            return;
        }

        let BindingPattern::BindingIdentifier(identifier) = &mut declarator.id else {
            return;
        };
        if identifier.name.len() > MINIFIED_IDENTIFIER_THRESHOLD {
            return;
        }

        let old_name = identifier.name.as_str().to_string();
        self.rename_binding_to_generated(identifier, &format!("{old_name}Ref"));
    }

    fn rename_property(&mut self, property: &mut BindingProperty<'a>) {
        if property.computed {
            return;
        }

        let Some(key_name) = binding_property_key_name(&property.key) else {
            return;
        };

        if property.shorthand {
            return;
        }

        let Some(binding) = property_binding_identifier_mut(&mut property.value) else {
            return;
        };

        let old_name = binding.name.as_str().to_string();
        if key_name == old_name {
            property.shorthand = true;
            return;
        }

        let Some(symbol_id) = binding.symbol_id.get() else {
            return;
        };
        let scope_id = self.scoping.symbol_scope_id(symbol_id);
        let new_name = self.generate_target_name(scope_id, &key_name);

        self.record_symbol_rename(symbol_id, scope_id, &new_name);
        binding.name = self.ast.ident(&new_name);
        property.shorthand = new_name == key_name;
    }

    fn rename_use_state_setter(&mut self, declarator: &mut VariableDeclarator<'a>) {
        let Some(Expression::CallExpression(call)) = declarator.init.as_ref() else {
            return;
        };
        if !is_react_call(call, "useState") || call.arguments.len() > 1 {
            return;
        }

        let BindingPattern::ArrayPattern(pattern) = &mut declarator.id else {
            return;
        };
        if pattern.elements.is_empty() || pattern.elements.len() > 2 {
            return;
        }

        if !is_optional_binding_identifier_or_hole(pattern.elements.first()) {
            return;
        }

        let state_name =
            optional_binding_identifier_name(pattern.elements.first()).map(str::to_string);
        let Some(setter) = optional_binding_identifier_mut(pattern.elements.get_mut(1)) else {
            return;
        };

        let setter_name = setter.name.as_str().to_string();
        let base_name = state_name.as_deref().unwrap_or(setter_name.as_str());
        if base_name.len() > MINIFIED_IDENTIFIER_THRESHOLD {
            return;
        }

        let Some(symbol_id) = setter.symbol_id.get() else {
            return;
        };

        let scope_id = self.scoping.symbol_scope_id(symbol_id);
        let new_name =
            self.generate_target_name(scope_id, &format!("set{}", pascal_case(base_name)));

        self.record_symbol_rename(symbol_id, scope_id, &new_name);
        setter.name = self.ast.ident(&new_name);
    }

    fn rename_use_reducer_pair(&mut self, declarator: &mut VariableDeclarator<'a>) {
        let Some(call) = call_expression_init(declarator) else {
            return;
        };
        if !is_react_call(call, "useReducer") || !(2..=3).contains(&call.arguments.len()) {
            return;
        }

        let BindingPattern::ArrayPattern(pattern) = &mut declarator.id else {
            return;
        };
        if pattern.elements.len() != 2 {
            return;
        }

        let Some(state) = optional_binding_identifier_mut(pattern.elements.get_mut(0)) else {
            return;
        };
        if state.name.len() < MINIFIED_IDENTIFIER_THRESHOLD {
            let old_name = state.name.as_str().to_string();
            self.rename_binding_to_generated(state, &format!("{old_name}State"));
        }

        let Some(dispatch) = optional_binding_identifier_mut(pattern.elements.get_mut(1)) else {
            return;
        };
        if dispatch.name.len() < MINIFIED_IDENTIFIER_THRESHOLD {
            let old_name = dispatch.name.as_str().to_string();
            self.rename_binding_to_generated(dispatch, &format!("{old_name}Dispatch"));
        }
    }

    fn rename_forward_ref_params(&mut self, declarator: &mut VariableDeclarator<'a>) {
        let Some(call) = call_expression_init_mut(declarator) else {
            return;
        };
        if !is_react_call(call, "forwardRef") || call.arguments.len() != 1 {
            return;
        }

        let Some(params) = forwarded_ref_params_mut(call) else {
            return;
        };
        if params.items.len() != 2 || params.rest.is_some() {
            return;
        }

        let (props_params, ref_params) = params.items.split_at_mut(1);
        let BindingPattern::BindingIdentifier(props) = &mut props_params[0].pattern else {
            return;
        };
        let BindingPattern::BindingIdentifier(reference) = &mut ref_params[0].pattern else {
            return;
        };

        if props.name.len() < MINIFIED_IDENTIFIER_THRESHOLD {
            self.rename_binding_to_generated(props, "props");
        }
        if reference.name.len() < MINIFIED_IDENTIFIER_THRESHOLD {
            self.rename_binding_to_generated(reference, "ref");
        }
    }

    fn rename_binding_to_generated(&mut self, identifier: &mut BindingIdentifier<'a>, base: &str) {
        let Some(symbol_id) = identifier.symbol_id.get() else {
            return;
        };
        let scope_id = self.scoping.symbol_scope_id(symbol_id);
        let new_name = self.generate_target_name(scope_id, base);
        self.record_symbol_rename(symbol_id, scope_id, &new_name);
        identifier.name = self.ast.ident(&new_name);
    }

    fn record_symbol_rename(&mut self, symbol_id: SymbolId, scope_id: ScopeId, new_name: &str) {
        self.generated_names
            .entry(scope_id)
            .or_default()
            .insert(new_name.to_string());
        self.renames.insert(symbol_id, new_name.to_string());
    }

    fn generate_target_name(&self, scope_id: ScopeId, key_name: &str) -> String {
        let base = if is_valid_binding_identifier(key_name) {
            key_name.to_string()
        } else {
            format!("_{key_name}")
        };

        if !self.is_name_occupied(scope_id, &base) {
            return base;
        }

        let mut index = 1;
        loop {
            let candidate = format!("{base}_{index}");
            if !self.is_name_occupied(scope_id, &candidate) {
                return candidate;
            }
            index += 1;
        }
    }

    fn is_name_occupied(&self, mut scope_id: ScopeId, name: &str) -> bool {
        if self.scoping.find_binding(scope_id, name.into()).is_some() {
            return true;
        }

        loop {
            if self
                .generated_names
                .get(&scope_id)
                .is_some_and(|names| names.contains(name))
            {
                return true;
            }

            let Some(parent_scope_id) = self.scoping.scope_parent_id(scope_id) else {
                return false;
            };
            scope_id = parent_scope_id;
        }
    }
}

fn call_expression_init<'b, 'a>(
    declarator: &'b VariableDeclarator<'a>,
) -> Option<&'b CallExpression<'a>> {
    let Some(Expression::CallExpression(call)) = declarator.init.as_ref() else {
        return None;
    };
    Some(call)
}

fn call_expression_init_mut<'b, 'a>(
    declarator: &'b mut VariableDeclarator<'a>,
) -> Option<&'b mut CallExpression<'a>> {
    let Some(Expression::CallExpression(call)) = declarator.init.as_mut() else {
        return None;
    };
    Some(call)
}

fn is_react_call(call: &CallExpression, hook_name: &str) -> bool {
    is_callee_name(&call.callee, hook_name)
}

fn is_callee_name(expression: &Expression, name: &str) -> bool {
    match expression {
        Expression::Identifier(identifier) => identifier.name.as_str() == name,
        Expression::StaticMemberExpression(member) => member.property.name.as_str() == name,
        Expression::ParenthesizedExpression(parenthesized) => {
            is_callee_name(&parenthesized.expression, name)
        }
        _ => false,
    }
}

fn forwarded_ref_params_mut<'b, 'a>(
    call: &'b mut CallExpression<'a>,
) -> Option<&'b mut FormalParameters<'a>> {
    let argument = call.arguments.get_mut(0)?;
    match argument {
        Argument::ArrowFunctionExpression(arrow) => Some(&mut arrow.params),
        Argument::FunctionExpression(function) => Some(&mut function.params),
        _ => None,
    }
}

fn optional_binding_identifier_name<'a, 'b>(
    element: Option<&'b Option<BindingPattern<'a>>>,
) -> Option<&'b str> {
    let Some(Some(BindingPattern::BindingIdentifier(identifier))) = element else {
        return None;
    };
    Some(identifier.name.as_str())
}

fn is_optional_binding_identifier_or_hole(element: Option<&Option<BindingPattern>>) -> bool {
    matches!(
        element,
        None | Some(None) | Some(Some(BindingPattern::BindingIdentifier(_)))
    )
}

fn optional_binding_identifier_mut<'a, 'b>(
    element: Option<&'b mut Option<BindingPattern<'a>>>,
) -> Option<&'b mut BindingIdentifier<'a>> {
    let Some(Some(BindingPattern::BindingIdentifier(identifier))) = element else {
        return None;
    };
    Some(identifier)
}

fn pascal_case(name: &str) -> String {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };

    first.to_ascii_uppercase().to_string() + chars.as_str()
}

struct SymbolRenamer<'a, 's> {
    ast: AstBuilder<'a>,
    scoping: &'s Scoping,
    renames: HashMap<SymbolId, String>,
}

impl<'a> VisitMut<'a> for SymbolRenamer<'a, '_> {
    fn visit_binding_identifier(&mut self, identifier: &mut BindingIdentifier<'a>) {
        if let Some(new_name) = identifier
            .symbol_id
            .get()
            .and_then(|symbol_id| self.renames.get(&symbol_id))
        {
            identifier.name = self.ast.ident(new_name);
        }
    }

    fn visit_identifier_reference(&mut self, identifier: &mut IdentifierReference<'a>) {
        let Some(symbol_id) = identifier
            .reference_id
            .get()
            .and_then(|reference_id| self.scoping.get_reference(reference_id).symbol_id())
        else {
            return;
        };

        if let Some(new_name) = self.renames.get(&symbol_id) {
            identifier.name = self.ast.ident(new_name);
        }
    }
}

fn binding_property_key_name(key: &PropertyKey) -> Option<String> {
    match key {
        PropertyKey::StaticIdentifier(identifier) => Some(identifier.name.as_str().to_string()),
        PropertyKey::Identifier(identifier) => Some(identifier.name.as_str().to_string()),
        _ => None,
    }
}

fn property_binding_identifier_mut<'a, 'b>(
    pattern: &'b mut BindingPattern<'a>,
) -> Option<&'b mut BindingIdentifier<'a>> {
    match pattern {
        BindingPattern::BindingIdentifier(identifier) => Some(identifier),
        BindingPattern::AssignmentPattern(assignment) => match &mut assignment.left {
            BindingPattern::BindingIdentifier(identifier) => Some(identifier),
            _ => None,
        },
        _ => None,
    }
}

fn is_valid_binding_identifier(name: &str) -> bool {
    if is_reserved_identifier(name) {
        return false;
    }

    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    (first == '_' || first == '$' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch == '$' || ch.is_ascii_alphanumeric())
}

fn is_reserved_identifier(name: &str) -> bool {
    matches!(
        name,
        "arguments"
            | "await"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "null"
            | "return"
            | "static"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::define_ast_inline_test;

    #[test]
    fn renames_object_destructuring_aliases() {
        define_ast_inline_test(transform_ast)(
            "
const {
  gql: t,
  dispatchers: o,
  listener: i = noop,
  sameName: sameName
} = n;
o.delete(t, i);
",
            "
const { gql, dispatchers, listener = noop, sameName } = n;
dispatchers.delete(gql, listener);
",
        );
    }

    #[test]
    fn renames_object_destructuring_in_function_parameters() {
        define_ast_inline_test(transform_ast)(
            "
function foo({
  gql: t,
  dispatchers: o,
  listener: i
}) {
  o.delete(t, i);
}

const foo2 = ({
  gql: t,
  dispatchers: o,
  listener: i
}) => {
  t[o].delete(i);
}
",
            "
function foo({ gql, dispatchers, listener }) {
  dispatchers.delete(gql, listener);
}
const foo2 = ({ gql, dispatchers, listener }) => {
  gql[dispatchers].delete(listener);
};
",
        );
    }

    #[test]
    fn resolves_name_conflicts() {
        define_ast_inline_test(transform_ast)(
            "
const gql = 1;

function foo({
  gql: t,
  dispatchers: o,
  listener: i
}, {
  gql: a,
  dispatchers: b,
  listener: c
}) {
  o.delete(t, i, a, b, c);
}
",
            "
const gql = 1;
function foo({ gql: gql_1, dispatchers, listener }, { gql: gql_2, dispatchers: dispatchers_1, listener: listener_1 }) {
  dispatchers.delete(gql_1, listener, gql_2, dispatchers_1, listener_1);
}
",
        );
    }

    #[test]
    fn prefixes_reserved_binding_names() {
        define_ast_inline_test(transform_ast)(
            "
const {
  static: t,
  default: o,
} = n;
o.delete(t);
",
            "
const { static: _static, default: _default } = n;
_default.delete(_static);
",
        );
    }

    #[test]
    fn react_renames_use_state_setters() {
        define_ast_inline_test(transform_ast)(
            "
const [e, f] = useState();
const [, g] = o.useState(0);
const [value, h] = useState(1);
const [{ value: i }, j] = useState();

const k = o.useState(a, b);
",
            "
const [e, setE] = useState();
const [, setG] = o.useState(0);
const [value, h] = useState(1);
const [{ value: value_1 }, j] = useState();
const k = o.useState(a, b);
",
        );
    }

    #[test]
    fn react_renames_create_context_bindings() {
        define_ast_inline_test(transform_ast)(
            "
const d = createContext(null);
const ef = o.createContext('light');
const g = o.createContext(a, b, c);
const ThemeContext = o.createContext('light');
",
            "
const DContext = createContext(null);
const EfContext = o.createContext(\"light\");
const g = o.createContext(a, b, c);
const ThemeContext = o.createContext(\"light\");
",
        );
    }

    #[test]
    fn react_renames_use_ref_bindings() {
        define_ast_inline_test(transform_ast)(
            "
const d = useRef();
const ef = o.useRef(null);
const g = o.useRef(a, b);
const buttonRef = o.useRef(null);
",
            "
const dRef = useRef();
const efRef = o.useRef(null);
const g = o.useRef(a, b);
const buttonRef = o.useRef(null);
",
        );
    }

    #[test]
    fn react_renames_use_reducer_bindings() {
        define_ast_inline_test(transform_ast)(
            "
const [e, f] = useReducer(r, i);
const [g, h] = o.useReducer(r, i, init);
const [state, j] = useReducer(r, i);
const [k, dispatch] = useReducer(r, i);
const [l, m] = o.useReducer(a);
",
            "
const [eState, fDispatch] = useReducer(r, i);
const [gState, hDispatch] = o.useReducer(r, i, init);
const [state, jDispatch] = useReducer(r, i);
const [kState, dispatch] = useReducer(r, i);
const [l, m] = o.useReducer(a);
",
        );
    }

    #[test]
    fn react_renames_forward_ref_params() {
        define_ast_inline_test(transform_ast)(
            "
const Z = forwardRef((e, t) => {
  return e.label + t.current;
});
const X = o.forwardRef(function (e, ref2) {
  return e.label + ref2.current;
});
const Y = o.forwardRef(a, b);
",
            "
const Z = forwardRef((props, ref) => {
  return props.label + ref.current;
});
const X = o.forwardRef(function(props, ref2) {
  return props.label + ref2.current;
});
const Y = o.forwardRef(a, b);
",
        );
    }
}
