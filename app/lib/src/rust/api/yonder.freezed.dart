// GENERATED CODE - DO NOT MODIFY BY HAND
// coverage:ignore-file
// ignore_for_file: type=lint, type=warning, deprecated_member_use, deprecated_member_use_from_same_package
// ignore_for_file: unused_element, deprecated_member_use, deprecated_member_use_from_same_package, use_function_type_syntax_for_parameters, unnecessary_const, avoid_init_to_null, invalid_override_different_default_values_named, prefer_expression_function_bodies, annotate_overrides, invalid_annotation_target, unnecessary_question_mark

part of 'yonder.dart';

// **************************************************************************
// FreezedGenerator
// **************************************************************************

// GENERATED CODE - DO NOT MODIFY BY HAND
// dart format off
T _$identity<T>(T value) => value;
/// @nodoc
mixin _$LogEvent {





@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is LogEvent);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
    return 'LogEvent()';
}


}

/// @nodoc
class $LogEventCopyWith<$Res>  {
$LogEventCopyWith(LogEvent _, $Res Function(LogEvent) __);
}


/// Adds pattern-matching-related methods to [LogEvent].
extension LogEventPatterns on LogEvent {
/// A variant of `map` that fallback to returning `orElse`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeMap<TResult extends Object?>({TResult Function( LogEvent_Output value)?  output,TResult Function( LogEvent_Finished value)?  finished,TResult Function( LogEvent_LentGitLogin value)?  lentGitLogin,required TResult orElse(),}){
final _that = this;
switch (_that) {
case LogEvent_Output() when output != null:
return output(_that);case LogEvent_Finished() when finished != null:
return finished(_that);case LogEvent_LentGitLogin() when lentGitLogin != null:
return lentGitLogin(_that);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// Callbacks receives the raw object, upcasted.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case final Subclass2 value:
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult map<TResult extends Object?>({required TResult Function( LogEvent_Output value)  output,required TResult Function( LogEvent_Finished value)  finished,required TResult Function( LogEvent_LentGitLogin value)  lentGitLogin,}){
final _that = this;
switch (_that) {
case LogEvent_Output():
return output(_that);case LogEvent_Finished():
return finished(_that);case LogEvent_LentGitLogin():
return lentGitLogin(_that);}
}
/// A variant of `map` that fallback to returning `null`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? mapOrNull<TResult extends Object?>({TResult? Function( LogEvent_Output value)?  output,TResult? Function( LogEvent_Finished value)?  finished,TResult? Function( LogEvent_LentGitLogin value)?  lentGitLogin,}){
final _that = this;
switch (_that) {
case LogEvent_Output() when output != null:
return output(_that);case LogEvent_Finished() when finished != null:
return finished(_that);case LogEvent_LentGitLogin() when lentGitLogin != null:
return lentGitLogin(_that);case _:
  return null;

}
}
/// A variant of `when` that fallback to an `orElse` callback.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeWhen<TResult extends Object?>({TResult Function( BigInt offset,  Uint8List data)?  output,TResult Function( Job job)?  finished,TResult Function( String host,  bool lent)?  lentGitLogin,required TResult orElse(),}) {final _that = this;
switch (_that) {
case LogEvent_Output() when output != null:
return output(_that.offset,_that.data);case LogEvent_Finished() when finished != null:
return finished(_that.job);case LogEvent_LentGitLogin() when lentGitLogin != null:
return lentGitLogin(_that.host,_that.lent);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// As opposed to `map`, this offers destructuring.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case Subclass2(:final field2):
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult when<TResult extends Object?>({required TResult Function( BigInt offset,  Uint8List data)  output,required TResult Function( Job job)  finished,required TResult Function( String host,  bool lent)  lentGitLogin,}) {final _that = this;
switch (_that) {
case LogEvent_Output():
return output(_that.offset,_that.data);case LogEvent_Finished():
return finished(_that.job);case LogEvent_LentGitLogin():
return lentGitLogin(_that.host,_that.lent);}
}
/// A variant of `when` that fallback to returning `null`
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? whenOrNull<TResult extends Object?>({TResult? Function( BigInt offset,  Uint8List data)?  output,TResult? Function( Job job)?  finished,TResult? Function( String host,  bool lent)?  lentGitLogin,}) {final _that = this;
switch (_that) {
case LogEvent_Output() when output != null:
return output(_that.offset,_that.data);case LogEvent_Finished() when finished != null:
return finished(_that.job);case LogEvent_LentGitLogin() when lentGitLogin != null:
return lentGitLogin(_that.host,_that.lent);case _:
  return null;

}
}

}

/// @nodoc


class LogEvent_Output extends LogEvent {
  const LogEvent_Output({required this.offset, required this.data}): super._();
  

 final  BigInt offset;
 final  Uint8List data;

/// Create a copy of LogEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$LogEvent_OutputCopyWith<LogEvent_Output> get copyWith => _$LogEvent_OutputCopyWithImpl<LogEvent_Output>(this, _$identity);



@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is LogEvent_Output&&(identical(other.offset, offset) || other.offset == offset)&&const DeepCollectionEquality().equals(other.data, data));
}


@override
int get hashCode {
    return Object.hash(runtimeType,offset,const DeepCollectionEquality().hash(data));
}

@override
String toString() {
    return 'LogEvent.output(offset: $offset, data: $data)';
}


}

/// @nodoc
abstract mixin class $LogEvent_OutputCopyWith<$Res> implements $LogEventCopyWith<$Res> {
  factory $LogEvent_OutputCopyWith(LogEvent_Output value, $Res Function(LogEvent_Output) _then) = _$LogEvent_OutputCopyWithImpl;
@useResult
$Res call({
 BigInt offset, Uint8List data
});




}
/// @nodoc
class _$LogEvent_OutputCopyWithImpl<$Res>
    implements $LogEvent_OutputCopyWith<$Res> {
  _$LogEvent_OutputCopyWithImpl(this._self, this._then);

  final LogEvent_Output _self;
  final $Res Function(LogEvent_Output) _then;

/// Create a copy of LogEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? offset = null,Object? data = null,}) {
  return _then(LogEvent_Output(
offset: null == offset ? _self.offset : offset // ignore: cast_nullable_to_non_nullable
as BigInt,data: null == data ? _self.data : data // ignore: cast_nullable_to_non_nullable
as Uint8List,
  ));
}


}

/// @nodoc


class LogEvent_Finished extends LogEvent {
  const LogEvent_Finished({required this.job}): super._();
  

 final  Job job;

/// Create a copy of LogEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$LogEvent_FinishedCopyWith<LogEvent_Finished> get copyWith => _$LogEvent_FinishedCopyWithImpl<LogEvent_Finished>(this, _$identity);



@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is LogEvent_Finished&&(identical(other.job, job) || other.job == job));
}


@override
int get hashCode {
    return Object.hash(runtimeType,job);
}

@override
String toString() {
    return 'LogEvent.finished(job: $job)';
}


}

/// @nodoc
abstract mixin class $LogEvent_FinishedCopyWith<$Res> implements $LogEventCopyWith<$Res> {
  factory $LogEvent_FinishedCopyWith(LogEvent_Finished value, $Res Function(LogEvent_Finished) _then) = _$LogEvent_FinishedCopyWithImpl;
@useResult
$Res call({
 Job job
});




}
/// @nodoc
class _$LogEvent_FinishedCopyWithImpl<$Res>
    implements $LogEvent_FinishedCopyWith<$Res> {
  _$LogEvent_FinishedCopyWithImpl(this._self, this._then);

  final LogEvent_Finished _self;
  final $Res Function(LogEvent_Finished) _then;

/// Create a copy of LogEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? job = null,}) {
  return _then(LogEvent_Finished(
job: null == job ? _self.job : job // ignore: cast_nullable_to_non_nullable
as Job,
  ));
}


}

/// @nodoc


class LogEvent_LentGitLogin extends LogEvent {
  const LogEvent_LentGitLogin({required this.host, required this.lent}): super._();
  

 final  String host;
 final  bool lent;

/// Create a copy of LogEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$LogEvent_LentGitLoginCopyWith<LogEvent_LentGitLogin> get copyWith => _$LogEvent_LentGitLoginCopyWithImpl<LogEvent_LentGitLogin>(this, _$identity);



@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is LogEvent_LentGitLogin&&(identical(other.host, host) || other.host == host)&&(identical(other.lent, lent) || other.lent == lent));
}


@override
int get hashCode {
    return Object.hash(runtimeType,host,lent);
}

@override
String toString() {
    return 'LogEvent.lentGitLogin(host: $host, lent: $lent)';
}


}

/// @nodoc
abstract mixin class $LogEvent_LentGitLoginCopyWith<$Res> implements $LogEventCopyWith<$Res> {
  factory $LogEvent_LentGitLoginCopyWith(LogEvent_LentGitLogin value, $Res Function(LogEvent_LentGitLogin) _then) = _$LogEvent_LentGitLoginCopyWithImpl;
@useResult
$Res call({
 String host, bool lent
});




}
/// @nodoc
class _$LogEvent_LentGitLoginCopyWithImpl<$Res>
    implements $LogEvent_LentGitLoginCopyWith<$Res> {
  _$LogEvent_LentGitLoginCopyWithImpl(this._self, this._then);

  final LogEvent_LentGitLogin _self;
  final $Res Function(LogEvent_LentGitLogin) _then;

/// Create a copy of LogEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? host = null,Object? lent = null,}) {
  return _then(LogEvent_LentGitLogin(
host: null == host ? _self.host : host // ignore: cast_nullable_to_non_nullable
as String,lent: null == lent ? _self.lent : lent // ignore: cast_nullable_to_non_nullable
as bool,
  ));
}


}

/// @nodoc
mixin _$ShellEvent {





@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is ShellEvent);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
    return 'ShellEvent()';
}


}

/// @nodoc
class $ShellEventCopyWith<$Res>  {
$ShellEventCopyWith(ShellEvent _, $Res Function(ShellEvent) __);
}


/// Adds pattern-matching-related methods to [ShellEvent].
extension ShellEventPatterns on ShellEvent {
/// A variant of `map` that fallback to returning `orElse`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeMap<TResult extends Object?>({TResult Function( ShellEvent_Output value)?  output,TResult Function( ShellEvent_Exited value)?  exited,TResult Function( ShellEvent_Disconnected value)?  disconnected,required TResult orElse(),}){
final _that = this;
switch (_that) {
case ShellEvent_Output() when output != null:
return output(_that);case ShellEvent_Exited() when exited != null:
return exited(_that);case ShellEvent_Disconnected() when disconnected != null:
return disconnected(_that);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// Callbacks receives the raw object, upcasted.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case final Subclass2 value:
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult map<TResult extends Object?>({required TResult Function( ShellEvent_Output value)  output,required TResult Function( ShellEvent_Exited value)  exited,required TResult Function( ShellEvent_Disconnected value)  disconnected,}){
final _that = this;
switch (_that) {
case ShellEvent_Output():
return output(_that);case ShellEvent_Exited():
return exited(_that);case ShellEvent_Disconnected():
return disconnected(_that);}
}
/// A variant of `map` that fallback to returning `null`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? mapOrNull<TResult extends Object?>({TResult? Function( ShellEvent_Output value)?  output,TResult? Function( ShellEvent_Exited value)?  exited,TResult? Function( ShellEvent_Disconnected value)?  disconnected,}){
final _that = this;
switch (_that) {
case ShellEvent_Output() when output != null:
return output(_that);case ShellEvent_Exited() when exited != null:
return exited(_that);case ShellEvent_Disconnected() when disconnected != null:
return disconnected(_that);case _:
  return null;

}
}
/// A variant of `when` that fallback to an `orElse` callback.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeWhen<TResult extends Object?>({TResult Function( BigInt offset,  Uint8List data)?  output,TResult Function( int? code)?  exited,TResult Function( String message)?  disconnected,required TResult orElse(),}) {final _that = this;
switch (_that) {
case ShellEvent_Output() when output != null:
return output(_that.offset,_that.data);case ShellEvent_Exited() when exited != null:
return exited(_that.code);case ShellEvent_Disconnected() when disconnected != null:
return disconnected(_that.message);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// As opposed to `map`, this offers destructuring.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case Subclass2(:final field2):
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult when<TResult extends Object?>({required TResult Function( BigInt offset,  Uint8List data)  output,required TResult Function( int? code)  exited,required TResult Function( String message)  disconnected,}) {final _that = this;
switch (_that) {
case ShellEvent_Output():
return output(_that.offset,_that.data);case ShellEvent_Exited():
return exited(_that.code);case ShellEvent_Disconnected():
return disconnected(_that.message);}
}
/// A variant of `when` that fallback to returning `null`
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? whenOrNull<TResult extends Object?>({TResult? Function( BigInt offset,  Uint8List data)?  output,TResult? Function( int? code)?  exited,TResult? Function( String message)?  disconnected,}) {final _that = this;
switch (_that) {
case ShellEvent_Output() when output != null:
return output(_that.offset,_that.data);case ShellEvent_Exited() when exited != null:
return exited(_that.code);case ShellEvent_Disconnected() when disconnected != null:
return disconnected(_that.message);case _:
  return null;

}
}

}

/// @nodoc


class ShellEvent_Output extends ShellEvent {
  const ShellEvent_Output({required this.offset, required this.data}): super._();
  

 final  BigInt offset;
 final  Uint8List data;

/// Create a copy of ShellEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$ShellEvent_OutputCopyWith<ShellEvent_Output> get copyWith => _$ShellEvent_OutputCopyWithImpl<ShellEvent_Output>(this, _$identity);



@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is ShellEvent_Output&&(identical(other.offset, offset) || other.offset == offset)&&const DeepCollectionEquality().equals(other.data, data));
}


@override
int get hashCode {
    return Object.hash(runtimeType,offset,const DeepCollectionEquality().hash(data));
}

@override
String toString() {
    return 'ShellEvent.output(offset: $offset, data: $data)';
}


}

/// @nodoc
abstract mixin class $ShellEvent_OutputCopyWith<$Res> implements $ShellEventCopyWith<$Res> {
  factory $ShellEvent_OutputCopyWith(ShellEvent_Output value, $Res Function(ShellEvent_Output) _then) = _$ShellEvent_OutputCopyWithImpl;
@useResult
$Res call({
 BigInt offset, Uint8List data
});




}
/// @nodoc
class _$ShellEvent_OutputCopyWithImpl<$Res>
    implements $ShellEvent_OutputCopyWith<$Res> {
  _$ShellEvent_OutputCopyWithImpl(this._self, this._then);

  final ShellEvent_Output _self;
  final $Res Function(ShellEvent_Output) _then;

/// Create a copy of ShellEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? offset = null,Object? data = null,}) {
  return _then(ShellEvent_Output(
offset: null == offset ? _self.offset : offset // ignore: cast_nullable_to_non_nullable
as BigInt,data: null == data ? _self.data : data // ignore: cast_nullable_to_non_nullable
as Uint8List,
  ));
}


}

/// @nodoc


class ShellEvent_Exited extends ShellEvent {
  const ShellEvent_Exited({this.code}): super._();
  

 final  int? code;

/// Create a copy of ShellEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$ShellEvent_ExitedCopyWith<ShellEvent_Exited> get copyWith => _$ShellEvent_ExitedCopyWithImpl<ShellEvent_Exited>(this, _$identity);



@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is ShellEvent_Exited&&(identical(other.code, code) || other.code == code));
}


@override
int get hashCode {
    return Object.hash(runtimeType,code);
}

@override
String toString() {
    return 'ShellEvent.exited(code: $code)';
}


}

/// @nodoc
abstract mixin class $ShellEvent_ExitedCopyWith<$Res> implements $ShellEventCopyWith<$Res> {
  factory $ShellEvent_ExitedCopyWith(ShellEvent_Exited value, $Res Function(ShellEvent_Exited) _then) = _$ShellEvent_ExitedCopyWithImpl;
@useResult
$Res call({
 int? code
});




}
/// @nodoc
class _$ShellEvent_ExitedCopyWithImpl<$Res>
    implements $ShellEvent_ExitedCopyWith<$Res> {
  _$ShellEvent_ExitedCopyWithImpl(this._self, this._then);

  final ShellEvent_Exited _self;
  final $Res Function(ShellEvent_Exited) _then;

/// Create a copy of ShellEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? code = freezed,}) {
  return _then(ShellEvent_Exited(
code: freezed == code ? _self.code : code // ignore: cast_nullable_to_non_nullable
as int?,
  ));
}


}

/// @nodoc


class ShellEvent_Disconnected extends ShellEvent {
  const ShellEvent_Disconnected({required this.message}): super._();
  

 final  String message;

/// Create a copy of ShellEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$ShellEvent_DisconnectedCopyWith<ShellEvent_Disconnected> get copyWith => _$ShellEvent_DisconnectedCopyWithImpl<ShellEvent_Disconnected>(this, _$identity);



@override
bool operator ==(Object other) {
    return identical(this, other) || (other.runtimeType == runtimeType&&other is ShellEvent_Disconnected&&(identical(other.message, message) || other.message == message));
}


@override
int get hashCode {
    return Object.hash(runtimeType,message);
}

@override
String toString() {
    return 'ShellEvent.disconnected(message: $message)';
}


}

/// @nodoc
abstract mixin class $ShellEvent_DisconnectedCopyWith<$Res> implements $ShellEventCopyWith<$Res> {
  factory $ShellEvent_DisconnectedCopyWith(ShellEvent_Disconnected value, $Res Function(ShellEvent_Disconnected) _then) = _$ShellEvent_DisconnectedCopyWithImpl;
@useResult
$Res call({
 String message
});




}
/// @nodoc
class _$ShellEvent_DisconnectedCopyWithImpl<$Res>
    implements $ShellEvent_DisconnectedCopyWith<$Res> {
  _$ShellEvent_DisconnectedCopyWithImpl(this._self, this._then);

  final ShellEvent_Disconnected _self;
  final $Res Function(ShellEvent_Disconnected) _then;

/// Create a copy of ShellEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? message = null,}) {
  return _then(ShellEvent_Disconnected(
message: null == message ? _self.message : message // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

// dart format on
